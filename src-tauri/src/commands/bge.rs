use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

const MODEL_DIR_NAME: &str = "bge-small-zh-v1.5";
const MODEL_FILE: &str = "model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

const HF_MIRROR_BASE: &str =
    "https://hf-mirror.com/BAAI/bge-small-zh-v1.5/resolve/main";
const MODELSCOPE_BASE: &str =
    "https://modelscope.cn/models/BAAI/bge-small-zh-v1.5/resolve/master";

static INSTALLING: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Clone)]
pub struct BgeInstallResult {
    pub success: bool,
    pub message: String,
}

#[derive(Serialize)]
pub struct BgeStatus {
    pub installed: bool,
    pub model_dir: String,
}

/// Download BGE embedding model into `data_dir/models/bge-small-zh-v1.5/`.
/// Runs on a background thread; emits `bge-install-done` on completion.
#[tauri::command]
pub fn install_bge(state: State<'_, AppState>, app: AppHandle) -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("BGE 模型下载已在后台进行中".into());
    }

    let model_dir = state
        .data_dir
        .join("models")
        .join(MODEL_DIR_NAME);
    std::fs::create_dir_all(&model_dir).map_err(|e| format!("创建目录失败: {e}"))?;

    std::thread::spawn(move || run_download(model_dir, app));
    Ok(())
}

fn run_download(model_dir: PathBuf, app: AppHandle) {
    let result = download_inner(&model_dir);
    INSTALLING.store(false, Ordering::SeqCst);
    let _ = app.emit("bge-install-done", &result);
}

/// Check whether BGE model files exist and are ready for use.
#[tauri::command]
pub fn check_bge_installed(state: State<'_, AppState>) -> Result<BgeStatus, String> {
    let model_dir = state
        .data_dir
        .join("models")
        .join(MODEL_DIR_NAME);
    Ok(BgeStatus {
        installed: bge_model_ready(&model_dir),
        model_dir: model_dir.display().to_string(),
    })
}

/// Check if both required model files exist.
pub fn bge_model_ready(model_dir: &Path) -> bool {
    model_dir.join(MODEL_FILE).is_file() && model_dir.join(TOKENIZER_FILE).is_file()
}

/// Download model.onnx then tokenizer.json, one at a time with mirror
/// fallback. Cleans up partial files on failure.
fn download_inner(model_dir: &Path) -> BgeInstallResult {
    log::info!("[BGE] 开始下载 BGE 模型到 {}", model_dir.display());

    let forced_mirror = matches!(
        std::env::var("LINK_SEARCHER_BGE_MIRROR").as_deref(),
        Ok("modelscope")
    );

    let files = [MODEL_FILE, TOKENIZER_FILE];
    for file_name in &files {
        let dest = model_dir.join(file_name);
        if dest.is_file() {
            log::info!("[BGE] {} 已存在，跳过", file_name);
            continue;
        }

        let base_urls = if forced_mirror {
            vec![MODELSCOPE_BASE.to_string()]
        } else {
            vec![
                HF_MIRROR_BASE.to_string(),
                MODELSCOPE_BASE.to_string(),
            ]
        };

        let mut downloaded = false;
        for (i, base) in base_urls.iter().enumerate() {
            let url = format!("{}/{}", base, file_name);
            log::info!("[BGE] 下载 {} ({})", file_name, url);
            match download(&url, &dest) {
                Ok(()) => {
                    downloaded = true;
                    break;
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&dest);
                    if i + 1 == base_urls.len() {
                        let msg = format!("下载 {} 失败: {e}", file_name);
                        log::error!("[BGE] {msg}");
                        return BgeInstallResult { success: false, message: msg };
                    }
                    log::warn!("[BGE] 源 {} 失败({e})，切换到镜像重试", i + 1);
                }
            }
        }
        if !downloaded {
            return BgeInstallResult {
                success: false,
                message: format!("下载 {} 失败", file_name),
            };
        }
    }

    if bge_model_ready(model_dir) {
        let size: u64 = files
            .iter()
            .filter_map(|f| std::fs::metadata(model_dir.join(f)).ok().map(|m| m.len()))
            .sum();
        let size_mb = size as f64 / (1024.0 * 1024.0);
        log::info!(
            "[BGE] 模型就绪: {} ({:.1} MB)",
            model_dir.display(),
            size_mb
        );
        BgeInstallResult {
            success: true,
            message: format!("BGE 模型下载完成（{:.1} MB）", size_mb),
        }
    } else {
        let msg = format!("模型文件不完整: {}", model_dir.display());
        log::error!("[BGE] {msg}");
        BgeInstallResult { success: false, message: msg }
    }
}

/// Stream a URL to a file. Uses ureq with proxy support; aborts after
/// `PER_SOURCE_TIMEOUT` so a stall on one source falls back to the mirror.
fn download(url: &str, dest: &Path) -> Result<(), String> {
    const PER_SOURCE_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(120);

    let resp = ureq::builder()
        .try_proxy_from_env(true)
        .timeout(PER_SOURCE_TIMEOUT)
        .build()
        .get(url)
        .call()
        .map_err(|e| format!("{e}"))?;

    let total = resp
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let mut reader = resp.into_reader();
    let mut file = File::create(dest).map_err(|e| format!("{e}"))?;
    let mut buf = [0u8; 64 * 1024];
    let mut written = 0u64;
    let mut last_pct = 0u32;
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > PER_SOURCE_TIMEOUT {
            let _ = std::fs::remove_file(dest);
            return Err(format!(
                "下载超时（{}s），已停止",
                PER_SOURCE_TIMEOUT.as_secs()
            ));
        }
        let n = reader.read(&mut buf).map_err(|e| format!("{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("{e}"))?;
        written += n as u64;
        if total > 0 {
            let pct = (written * 100 / total) as u32;
            if pct - last_pct >= 5 {
                last_pct = pct;
                log::info!(
                    "[BGE] 下载 {}% ({:.1}/{:.1} MB, {:.1} MB/s)",
                    pct,
                    written as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0,
                    written as f64 / 1_048_576.0
                        / started.elapsed().as_secs_f64().max(0.1),
                );
            }
        }
    }
    Ok(())
}
