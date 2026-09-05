//! Tauri commands for the dependency installer.
//!
//! - `get_setup_status` — snapshot of every tracked dep (wizard gate + dep
//!   center).
//! - `install_dep` — start a background install for one dep id.
//! - `cancel_dep_install` — cancel the running install.
//!
//! Progress is streamed to the frontend as `dep-progress`
//! `{ dep, current, total, bytes }` and completion as `dep-install-done`
//! `{ dep, success, message }`. Only one dep installs at a time (mirrors the
//! existing FunASR/BGE re-entry guard).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::deps::{catalog, download};
use crate::state::AppState;

/// Single-flight guard: at most one dep install at a time.
static INSTALLING_DEP: AtomicBool = AtomicBool::new(false);
/// Cooperative cancel flag shared with the running download thread.
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Which dep is currently installing (for UI disable + cancel).
static CURRENT_DEP: Mutex<Option<String>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
pub struct DepInstallResult {
    pub dep: String,
    pub success: bool,
    pub message: String,
}

#[tauri::command]
pub fn get_setup_status(state: State<'_, AppState>) -> Result<crate::deps::SetupStatus, String> {
    Ok(crate::deps::current_status(&state))
}

#[tauri::command]
pub fn install_dep(
    state: State<'_, AppState>,
    app: AppHandle,
    dep: String,
) -> Result<(), String> {
    let Some(def) = catalog::all().into_iter().find(|d| d.id == dep) else {
        return Err(format!("未知依赖: {dep}"));
    };
    let is_system_pkg = def.files.is_empty() && def.system_package.is_some();
    if def.files.is_empty() && !is_system_pkg {
        // System-provided deps with no package spec (future deps) — not installable.
        return Err(format!("依赖 {dep} 需要按平台手动安装（见引导）"));
    }
    if INSTALLING_DEP.swap(true, Ordering::SeqCst) {
        return Err("已有依赖正在安装中".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    *CURRENT_DEP.lock().unwrap_or_else(|e| e.into_inner()) = Some(dep.clone());

    let data_dir = state.data_dir.clone();
    std::thread::spawn(move || {
        let app_for_progress = app.clone();
        let dep_for_progress = dep.clone();
        let on_progress = move |cur: u64, total: u64, bytes: u64| {
            let _ = app_for_progress.emit(
                "dep-progress",
                serde_json::json!({
                    "dep": dep_for_progress,
                    "current": cur,
                    "total": total,
                    "bytes": bytes,
                }),
            );
        };
        let res = if is_system_pkg {
            install_system_package(&def)
        } else {
            download::install_dep(&def, &data_dir, &CANCEL, &on_progress)
        };
        let cancelled = CANCEL.load(Ordering::SeqCst);
        let result = match res {
            Ok(()) => DepInstallResult {
                dep: dep.clone(),
                success: true,
                message: "安装完成".into(),
            },
            Err(e) => DepInstallResult {
                dep: dep.clone(),
                success: false,
                message: if cancelled {
                    "已取消".into()
                } else {
                    e.0
                },
            },
        };

        INSTALLING_DEP.store(false, Ordering::SeqCst);
        CANCEL.store(false, Ordering::SeqCst);
        *CURRENT_DEP.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = app.emit("dep-install-done", &result);
    });

    Ok(())
}

/// Install a system-provided dep via the platform package manager
/// (winget on Windows, brew on macOS, apt on Debian/Ubuntu).
fn install_system_package(def: &catalog::DepDef) -> Result<(), download::DownloadError> {
    let Some(pkg) = &def.system_package else {
        return Err(download::DownloadError(format!("依赖 {} 需要按平台手动安装", def.id)));
    };

    #[cfg(target_os = "windows")]
    let (cmd, args) = match pkg.winget {
        Some(id) => ("winget", vec!["install", "-e", "--id", id, "--silent", "--accept-package-agreements", "--accept-source-agreements"]),
        None => return Err(download::DownloadError("该依赖不支持 winget 安装".into())),
    };
    #[cfg(target_os = "macos")]
    let (cmd, args) = match pkg.brew {
        Some(p) => ("brew", vec!["install", p]),
        None => return Err(download::DownloadError("该依赖不支持 brew 安装".into())),
    };
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    let (cmd, args) = match pkg.apt {
        Some(p) => ("apt-get", vec!["install", "-y", p]),
        None => return Err(download::DownloadError("该依赖不支持 apt 安装".into())),
    };
    #[cfg(all(target_os = "linux", not(target_env = "gnu")))]
    let (cmd, args) = ("", Vec::<&str>::new());

    #[cfg(target_os = "linux")]
    let exit = std::process::Command::new("sudo")
        .arg(cmd)
        .args(&args)
        .status();
    #[cfg(not(target_os = "linux"))]
    let exit = std::process::Command::new(cmd)
        .args(&args)
        .status();

    let exit = exit.map_err(|e| download::DownloadError(format!("无法启动 {cmd}: {e}")))?;
    if exit.success() {
        Ok(())
    } else {
        Err(download::DownloadError(format!("{cmd} 安装失败（exit={}）", exit.code().unwrap_or(-1))))
    }
}

#[tauri::command]
pub fn cancel_dep_install() -> Result<(), String> {
    if !INSTALLING_DEP.load(Ordering::SeqCst) {
        return Err("当前没有正在进行的安装".into());
    }
    CANCEL.store(true, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
pub fn dep_install_status() -> Result<serde_json::Value, String> {
    let current = CURRENT_DEP
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Ok(serde_json::json!({
        "installing": INSTALLING_DEP.load(Ordering::SeqCst),
        "dep": current,
    }))
}
