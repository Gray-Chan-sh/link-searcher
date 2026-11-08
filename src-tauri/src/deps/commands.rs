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
    // npm 全局包（lark-cli）：既不是可下载文件，也不是 winget/brew/apt 包 —— 走 npm 分支。
    let is_npm_pkg = npm_package_of(def.id).is_some();
    if def.files.is_empty() && !is_system_pkg && !is_npm_pkg {
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
        let res = if is_npm_pkg {
            install_npm_package(&def)
        } else if is_system_pkg {
            install_system_package(&def)
        } else {
            download::install_dep(&def, &data_dir, &CANCEL, &on_progress)
        };
        // winget/brew/apt update PATH (winget: the registry; macOS:
        // path_helper). A running process keeps its launch-time PATH, so
        // re-read it before the frontend's post-install refresh — otherwise
        // the tool stays "not found".
        if res.is_ok() {
            crate::process::refresh_path();
            // 首次装完 PaddleOCR 后立刻把平铺目录登记为"当前版本"，
            // 这样远端清单的"要不要升级"从一开始就有基线。
            if dep == "paddleocr" {
                crate::extractor::paddleocr::adopt_flat_install(&data_dir);
            }
        }
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

/// npm 包名：id → 包名。**只此一处映射**（也用于判断"这是不是 npm 依赖"）。
///
/// 放在这里而不是给 `DepDef` 加字段：与就绪探测同一个取舍（见 `catalog::lark_cli`）——
/// 不为**单个**条目引入一整套来源类型，代价是这两处 id 特例必须同生共死。
fn npm_package_of(id: &str) -> Option<&'static str> {
    match id {
        "lark-cli" => Some("@larksuite/cli"),
        _ => None,
    }
}

/// 安装 npm 全局包（目前只有 `lark-cli`）。
///
/// 两个要点：
/// - **Windows 上必须经 `cmd /C`**：`npm` 只有 `.cmd` 批处理包装，而 `CreateProcess`
///   不能直接执行 `.cmd`（与 `lark-cli` 那条坑同源）。这里的参数是**固定字面量**
///   （包名来自本文件），不含任何用户输入，所以没有"被 cmd 二次解析"的风险。
/// - npm 不在 PATH 很常见（Node 装在非默认位置）→ 报错里给出**可执行的手动命令**。
fn install_npm_package(def: &catalog::DepDef) -> Result<(), download::DownloadError> {
    let Some(pkg) = npm_package_of(def.id) else {
        return Err(download::DownloadError(format!("依赖 {} 没有 npm 安装规格", def.id)));
    };

    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "npm", "install", "-g", pkg]);
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = std::process::Command::new("npm");
        c.args(["install", "-g", pkg]);
        c
    };

    let out = cmd.output().map_err(|e| {
        download::DownloadError(format!(
            "无法启动 npm：{e}。请先安装 Node.js（含 npm），或手动执行：npm i -g {pkg}"
        ))
    })?;
    if !out.status.success() {
        let detail: String = String::from_utf8_lossy(&out.stderr).trim().chars().take(400).collect();
        return Err(download::DownloadError(format!(
            "npm 安装失败（退出码 {:?}）：{detail}",
            out.status.code()
        )));
    }
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

// ---------------------------------------------------------------------------
// 模型在线升级（远端清单驱动，不发版换模型）
// ---------------------------------------------------------------------------

/// 「检查更新」的结果。`ok=false` 表示没拉到清单（网络问题），前端只提示不打扰。
#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheck {
    pub ok: bool,
    pub error: Option<String>,
    pub app_version: String,
    pub updates: Vec<crate::deps::remote::UpdateInfo>,
}

/// 检查各依赖是否有新版本。网络请求 5s 超时，失败**不报错**给用户
/// （返回 `ok=false`），避免离线状态下每次开依赖中心都弹错误。
#[tauri::command]
pub async fn check_dep_updates(
    state: State<'_, AppState>,
) -> Result<UpdateCheck, String> {
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let app_version = crate::deps::remote::app_version().to_string();
        match crate::deps::remote::fetch(std::time::Duration::from_secs(5)) {
            Ok(cat) => UpdateCheck {
                ok: true,
                error: None,
                app_version,
                updates: crate::deps::remote::pending_updates(&cat, &data_dir),
            },
            Err(e) => {
                log::info!("[DEPS] 检查更新失败（按无更新处理）: {e}");
                UpdateCheck { ok: false, error: Some(e), app_version, updates: Vec::new() }
            }
        }
    })
    .await
    .map_err(|e| format!("检查更新失败: {e}"))
}

/// 升级到远端清单里的最新版本（后台线程，进度复用 `dep-progress` 事件）。
///
/// 顺序刻意如此：下载到 `versions/.staging-<ver>` → **冒烟**（能检出文本）→
/// 原子改名进 `versions/<ver>` → 写状态 → 清理旧版本。任何一步失败都保持
/// 当前版本不变，只删 staging 目录。
#[tauri::command]
pub fn upgrade_dep(state: State<'_, AppState>, app: AppHandle, dep: String) -> Result<(), String> {
    if !crate::deps::remote::is_versioned(&dep) {
        return Err(format!("依赖 {dep} 暂不支持在线升级"));
    }
    if INSTALLING_DEP.swap(true, Ordering::SeqCst) {
        return Err("已有依赖正在安装中".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    *CURRENT_DEP.lock().unwrap_or_else(|e| e.into_inner()) = Some(dep.clone());

    let data_dir = state.data_dir.clone();
    std::thread::spawn(move || {
        let app_progress = app.clone();
        let dep_progress = dep.clone();
        let on_progress = move |cur: u64, total: u64, bytes: u64| {
            let _ = app_progress.emit(
                "dep-progress",
                serde_json::json!({
                    "dep": dep_progress,
                    "current": cur,
                    "total": total,
                    "bytes": bytes,
                }),
            );
        };

        let res = do_upgrade(&data_dir, &dep, &on_progress);
        let cancelled = CANCEL.load(Ordering::SeqCst);
        let result = match res {
            Ok(version) => DepInstallResult {
                dep: dep.clone(),
                success: true,
                message: format!("已升级到 {version}"),
            },
            Err(e) => DepInstallResult {
                dep: dep.clone(),
                success: false,
                message: if cancelled { "已取消".into() } else { e },
            },
        };

        INSTALLING_DEP.store(false, Ordering::SeqCst);
        CANCEL.store(false, Ordering::SeqCst);
        *CURRENT_DEP.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = app.emit("dep-install-done", &result);
    });

    Ok(())
}

pub(crate) fn do_upgrade(data_dir: &std::path::Path, dep: &str, on_progress: &download::ProgressFn) -> Result<String, String> {
    use crate::deps::{remote, state};

    let cat = remote::fetch(std::time::Duration::from_secs(8))
        .map_err(|e| format!("拉取模型清单失败: {e}"))?;
    let st = state::load(data_dir);
    let current = st.deps.get(dep).map(|e| e.current.clone());
    let plan = remote::plan_for(&cat, dep, current.as_deref())
        .ok_or_else(|| "没有可用的新版本".to_string())?;

    let files: Vec<download::FileTuple> = plan
        .files
        .iter()
        .map(|f| (f.remote.clone(), f.local.clone(), f.sha256.clone()))
        .collect();

    let staging = state::versions_root(data_dir, dep).join(format!(".staging-{}", plan.version));
    let _ = std::fs::remove_dir_all(&staging);

    let dl = download::install_into(
        &files,
        &plan.sources,
        &staging,
        &format!("{dep}@{}", plan.version),
        plan.size_bytes,
        &CANCEL,
        on_progress,
    );
    if let Err(e) = dl {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e.0);
    }

    // 冒烟：真正跑一遍推理，而不只是校验 sha256（历史教训：v6 在 tract 0.20 上
    // 校验全对、但输出全 blank）。
    if dep == "paddleocr" {
        match crate::extractor::paddleocr::verify_model_dir(&staging) {
            Ok(n) if n > 0 => {}
            Ok(_) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err("新模型自检没有检出任何文本区域，已放弃切换（旧版本保持不变）".into());
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(format!("新模型自检失败，已放弃切换: {e}"));
            }
        }
    }

    let final_dir = state::version_dir(data_dir, dep, &plan.version);
    let _ = std::fs::remove_dir_all(&final_dir);
    std::fs::rename(&staging, &final_dir)
        .map_err(|e| format!("提交新版本目录失败: {e}"))?;

    state::register_install(data_dir, dep, &plan.version)?;
    state::prune(data_dir, dep, Some(&plan.version));

    // 让应用立刻用上新模型（否则要重启才生效）
    if dep == "paddleocr" {
        crate::extractor::paddleocr::reload_engine_pool();
    }
    log::info!("[DEPS] {dep} 已升级到 {}（回退可用）", plan.version);
    Ok(plan.version)
}

/// 回退到上一版本（同样走冒烟 + 重启引擎池）。
#[tauri::command]
pub fn rollback_dep(state: State<'_, AppState>, app: AppHandle, dep: String) -> Result<(), String> {
    if !crate::deps::remote::is_versioned(&dep) {
        return Err(format!("依赖 {dep} 暂不支持版本回退"));
    }
    if INSTALLING_DEP.swap(true, Ordering::SeqCst) {
        return Err("已有依赖正在安装中".into());
    }
    *CURRENT_DEP.lock().unwrap_or_else(|e| e.into_inner()) = Some(dep.clone());
    let data_dir = state.data_dir.clone();
    std::thread::spawn(move || {
        let res = do_rollback(&data_dir, &dep);
        let result = match res {
            Ok(v) => DepInstallResult {
                dep: dep.clone(),
                success: true,
                message: format!("已回退到 {v}"),
            },
            Err(e) => DepInstallResult { dep: dep.clone(), success: false, message: e },
        };
        INSTALLING_DEP.store(false, Ordering::SeqCst);
        *CURRENT_DEP.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = app.emit("dep-install-done", &result);
    });
    Ok(())
}

fn do_rollback(data_dir: &std::path::Path, dep: &str) -> Result<String, String> {
    use crate::deps::state;
    let version = state::rollback(data_dir, dep, 2)?;
    if dep == "paddleocr" {
        let dir = state::version_dir(data_dir, dep, &version);
        crate::extractor::paddleocr::verify_model_dir(&dir)
            .map_err(|e| format!("回退后的模型自检失败: {e}"))?;
        crate::extractor::paddleocr::reload_engine_pool();
    }
    log::info!("[DEPS] {dep} 已回退到 {version}");
    Ok(version)
}

/// 启动后延迟检查并**静默**升级（仅在设置里打开 `auto_update_models` 时）。
///
/// 失败一律静默：断网、清单坏了、冒烟不通过 → 保持旧版本，只写日志。
/// 单独起线程 + 延迟 30s，避免和启动扫描抢 CPU / 网络。
pub fn auto_update(app: AppHandle, data_dir: std::path::PathBuf) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        if !crate::config::load_config().auto_update_models {
            return;
        }
        let cat = match crate::deps::remote::fetch(std::time::Duration::from_secs(8)) {
            Ok(c) => c,
            Err(e) => {
                log::info!("[DEPS] 自动检查更新失败（忽略）: {e}");
                return;
            }
        };
        let st = crate::deps::state::load(&data_dir);
        let current = st.deps.get("paddleocr").map(|e| e.current.clone());
        if crate::deps::remote::plan_for(&cat, "paddleocr", current.as_deref()).is_none() {
            return;
        }
        if INSTALLING_DEP.swap(true, Ordering::SeqCst) {
            log::info!("[DEPS] 自动升级跳过：已有安装任务在进行");
            return;
        }
        CANCEL.store(false, Ordering::SeqCst);
        *CURRENT_DEP.lock().unwrap_or_else(|e| e.into_inner()) = Some("paddleocr".into());

        let app_progress = app.clone();
        let on_progress = move |cur: u64, total: u64, bytes: u64| {
            let _ = app_progress.emit(
                "dep-progress",
                serde_json::json!({ "dep": "paddleocr", "current": cur, "total": total, "bytes": bytes }),
            );
        };
        let res = do_upgrade(&data_dir, "paddleocr", &on_progress);

        INSTALLING_DEP.store(false, Ordering::SeqCst);
        *CURRENT_DEP.lock().unwrap_or_else(|e| e.into_inner()) = None;
        match res {
            Ok(v) => {
                log::info!("[DEPS] 自动升级完成: paddleocr -> {v}");
                let _ = app.emit(
                    "dep-install-done",
                    &DepInstallResult { dep: "paddleocr".into(), success: true, message: format!("已自动升级到 {v}") },
                );
            }
            Err(e) => log::warn!("[DEPS] 自动升级失败（保持旧版本）: {e}"),
        }
    });
}
