use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

const MODEL_FILE: &str = "model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

/// (model_name, display_name, dim)
const LOCAL_MODELS: &[(&str, &str, u32)] = &[
    ("bge-small-zh-v1.5", "BGE-Small (512维)", 512),
    ("bge-large-zh-v1.5", "BGE-Large (1024维)", 1024),
];

fn remote_bases(model_name: &str) -> Vec<String> {
    vec![
        format!("https://hf-mirror.com/Xenova/{model_name}/resolve/main"),
        format!("https://modelscope.cn/models/Xenova/{model_name}/resolve/master"),
    ]
}

static INSTALLING: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Clone)]
pub struct BgeInstallResult {
    pub success: bool,
    pub message: String,
}

#[derive(Serialize, Clone)]
pub struct BgeStatus {
    pub installed: bool,
    pub model_dir: String,
    pub model_name: String,
}

#[tauri::command]
pub fn install_bge(state: State<'_, AppState>, app: AppHandle, model_name: Option<String>) -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("BGE 模型下载已在后台进行中".into());
    }

    let name = model_name.unwrap_or_else(|| {
        let cfg = crate::config::load_config();
        crate::ai::local_embed::local_model_dir_name(&cfg.active_embedding_model_id)
            .unwrap_or("bge-small-zh-v1.5")
            .to_string()
    });
    let model_dir = state.data_dir.join("models").join(&name);
    std::fs::create_dir_all(&model_dir).map_err(|e| format!("创建目录失败: {e}"))?;

    std::thread::spawn(move || run_download(model_dir, name, app));
    Ok(())
}

fn run_download(model_dir: PathBuf, model_name: String, app: AppHandle) {
    let result = download_inner(&model_dir, &model_name);
    INSTALLING.store(false, Ordering::SeqCst);
    let _ = app.emit("bge-install-done", &result);
}

#[tauri::command]
pub fn check_bge_installed(state: State<'_, AppState>) -> Result<Vec<BgeStatus>, String> {
    let mut statuses = Vec::new();
    for &(name, display, _dim) in LOCAL_MODELS {
        let model_dir = state.data_dir.join("models").join(name);
        statuses.push(BgeStatus {
            installed: bge_model_ready(&model_dir),
            model_dir: model_dir.display().to_string(),
            model_name: display.to_string(),
        });
    }
    Ok(statuses)
}

pub fn bge_model_ready(model_dir: &Path) -> bool {
    model_dir.join(MODEL_FILE).is_file() && model_dir.join(TOKENIZER_FILE).is_file()
}

fn download_inner(model_dir: &Path, model_name: &str) -> BgeInstallResult {
    log::info!("[BGE] 开始下载 {model_name} 到 {}", model_dir.display());

    let forced_mirror = matches!(
        std::env::var("LINK_SEARCHER_BGE_MIRROR").as_deref(),
        Ok("modelscope")
    );

    let files = [("onnx/model.onnx", MODEL_FILE), ("tokenizer.json", TOKENIZER_FILE)];
    for (remote, local) in &files {
        let dest = model_dir.join(local);
        if dest.is_file() {
            log::info!("[BGE] {} 已存在，跳过", local);
            continue;
        }

        let base_urls = if forced_mirror {
            vec![remote_bases(model_name).pop().unwrap()]
        } else {
            remote_bases(model_name)
        };

        let mut downloaded = false;
        for (i, base) in base_urls.iter().enumerate() {
            let url = format!("{}/{}", base, remote);
            log::info!("[BGE] 下载 {} ({})", local, url);
            match download(&url, &dest) {
                Ok(()) => {
                    downloaded = true;
                    break;
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&dest);
                    if i + 1 == base_urls.len() {
                        let msg = format!("下载 {} 失败: {e}", local);
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
                message: format!("下载 {} 失败", local),
            };
        }
    }

    if bge_model_ready(model_dir) {
        let size: u64 = [MODEL_FILE, TOKENIZER_FILE]
            .iter()
            .filter_map(|f| std::fs::metadata(model_dir.join(f)).ok().map(|m| m.len()))
            .sum();
        let size_mb = size as f64 / (1024.0 * 1024.0);
        log::info!(
            "[BGE] {model_name} 就绪: {} ({:.1} MB)",
            model_dir.display(),
            size_mb
        );
        BgeInstallResult {
            success: true,
            message: format!("{model_name} 下载完成（{:.1} MB）", size_mb),
        }
    } else {
        let msg = format!("模型文件不完整: {}", model_dir.display());
        log::error!("[BGE] {msg}");
        BgeInstallResult { success: false, message: msg }
    }
}

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
