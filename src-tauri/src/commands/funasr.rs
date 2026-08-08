use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

const MODEL_ARCHIVE: &str = "sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2";
const GITHUB_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/";
const MODELSCOPE_URL: &str =
    "https://modelscope.cn/models/csukuangfj/asr-models/resolve/master/";

static INSTALLING: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Clone)]
pub struct FunasrInstallResult {
    pub success: bool,
    pub message: String,
}

/// Download + extract the FunASR-Nano int8 ONNX model into
/// `data_dir/models/funasr`. Runs on a background thread so the UI stays
/// responsive; on completion an `funasr-install-done` event is emitted.
///
/// Archive layout (top-level dir `sherpa-onnx-funasr-nano-int8-2025-12-30/`):
///   encoder_adaptor.int8.onnx, llm.int8.onnx, embedding.int8.onnx,
///   Qwen3-0.6B/{merges.txt,tokenizer.json,vocab.json}, test_wavs/
///
/// After extraction the inner model dir is moved up so the layout matches
/// what `funasr_candidates()` in `extractor/audio.rs` expects.
#[tauri::command]
pub fn install_funasr(state: State<'_, AppState>, app: AppHandle) -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("FunASR 模型下载已在后台进行中".into());
    }

    let model_dir = state.data_dir.join("models").join("funasr");
    std::fs::create_dir_all(&model_dir).map_err(|e| format!("创建目录失败: {e}"))?;

    std::thread::spawn(move || run_download(model_dir, app));
    Ok(())
}

fn run_download(model_dir: PathBuf, app: AppHandle) {
    let result = download_inner(&model_dir);
    INSTALLING.store(false, Ordering::SeqCst);
    let _ = app.emit("funasr-install-done", &result);
}

/// Download sources in priority order. Default tries GitHub first and falls
/// back to the ModelScope mirror; `LINK_SEARCHER_FUNASR_MIRROR=modelscope`
/// forces mirror-only (for users who know GitHub is unreachable).
fn download_sources() -> Vec<String> {
    let forced_mirror = matches!(
        std::env::var("LINK_SEARCHER_FUNASR_MIRROR").as_deref(),
        Ok("modelscope")
    );
    if forced_mirror {
        vec![format!("{MODELSCOPE_URL}{MODEL_ARCHIVE}")]
    } else {
        vec![
            format!("{GITHUB_URL}{MODEL_ARCHIVE}"),
            format!("{MODELSCOPE_URL}{MODEL_ARCHIVE}"),
        ]
    }
}

fn download_inner(model_dir: &Path) -> FunasrInstallResult {
    log::info!("[FUNASR] 开始下载 ASR 模型到 {}", model_dir.display());

    let sources = download_sources();

    let archive_path = model_dir.join(MODEL_ARCHIVE);
    if !archive_path.exists() {
        for (i, url) in sources.iter().enumerate() {
            log::info!("[FUNASR] 下载 {} ({})", MODEL_ARCHIVE, url);
            match download(url, &archive_path) {
                Ok(()) => break,
                Err(e) => {
                    let _ = std::fs::remove_file(&archive_path);
                    if i + 1 == sources.len() {
                        let msg = format!("下载模型失败: {e}");
                        log::error!("[FUNASR] {msg}");
                        return FunasrInstallResult { success: false, message: msg };
                    }
                    log::warn!("[FUNASR] 源 {} 失败({e})，切换到镜像重试", i + 1);
                }
            }
        }
        log::info!("[FUNASR] 下载完成，开始解压");
    }

    let extracted_top = model_dir.join("sherpa-onnx-funasr-nano-int8-2025-12-30");
    let extracted = if extracted_top.join("encoder_adaptor.int8.onnx").exists() {
        extracted_top
    } else {
        let tmp = match extract_archive(&archive_path, model_dir) {
            Ok(d) => d,
            Err(e) => {
                let msg = format!("解压模型失败: {e}");
                log::error!("[FUNASR] {msg}");
                return FunasrInstallResult { success: false, message: msg };
            }
        };
        let _ = std::fs::remove_file(&archive_path);
        tmp
    };

    // Verify required files landed.
    let ok = ["encoder_adaptor.int8.onnx", "llm.int8.onnx", "embedding.int8.onnx", "Qwen3-0.6B/tokenizer.json"]
        .iter()
        .all(|f| extracted.join(f).is_file());
    if ok {
        let size = dir_size(&extracted) as f64 / (1024.0 * 1024.0 * 1024.0);
        log::info!("[FUNASR] 模型就绪: {} ({:.1} GiB)", extracted.display(), size);
        FunasrInstallResult { success: true, message: format!("FunASR 模型下载完成（{:.1} GB）", size) }
    } else {
        let msg = format!("模型文件不完整: {}", extracted.display());
        log::error!("[FUNASR] {msg}");
        FunasrInstallResult { success: false, message: msg }
    }
}

/// Stream a URL to a file. Uses a blocking socket; reports to the app log.
/// Aborts after `PER_SOURCE_TIMEOUT` so a stall on one source falls back
/// to the mirror instead of hanging the installer.
fn download(url: &str, dest: &Path) -> Result<(), String> {
    const PER_SOURCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

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
            return Err(format!("下载超时（{}s），已停止", PER_SOURCE_TIMEOUT.as_secs()));
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
                    "[FUNASR] 下载 {}% ({:.1}/{:.1} MB, {:.1} MB/s)",
                    pct,
                    written as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0,
                    written as f64 / 1_048_576.0 / started.elapsed().as_secs_f64().max(0.1),
                );
            }
        }
    }
    Ok(())
}

/// Extract a .tar.bz2 archive into `dest`, returning the single top-level
/// directory extracted. The archive is streamed (not fully buffered).
fn extract_archive(archive_path: &Path, dest: &Path) -> Result<PathBuf, String> {
    let file = File::open(archive_path).map_err(|e| format!("{e}"))?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut ar = tar::Archive::new(decoder);
    ar.unpack(dest).map_err(|e| format!("{e}"))?;

    let mut top: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dest).map_err(|e| format!("{e}"))? {
        let p = entry.map_err(|e| format!("{e}"))?.path();
        if p.is_dir() {
            top = Some(p);
            break;
        }
    }
    top.ok_or_else(|| "归档中未找到模型目录".to_string())
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        dir_size(&p)
                    } else {
                        std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_guard_prevents_reentry() {
        assert!(!INSTALLING.swap(true, Ordering::SeqCst));
        assert!(INSTALLING.swap(true, Ordering::SeqCst));
        INSTALLING.store(false, Ordering::SeqCst);
    }

    #[test]
    fn download_sources_modes() {
        // env-var mutation is not safe across parallel tests, so both modes
        // are asserted sequentially in this single test.
        unsafe { std::env::remove_var("LINK_SEARCHER_FUNASR_MIRROR") };
        let sources = download_sources();
        assert_eq!(sources.len(), 2);
        assert!(sources[0].contains("github.com"));
        assert!(sources[1].contains("modelscope.cn"));

        unsafe { std::env::set_var("LINK_SEARCHER_FUNASR_MIRROR", "modelscope") };
        let sources = download_sources();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].contains("modelscope.cn"));
        unsafe { std::env::remove_var("LINK_SEARCHER_FUNASR_MIRROR") };
    }
}
