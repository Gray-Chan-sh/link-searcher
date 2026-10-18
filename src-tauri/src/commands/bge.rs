use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

const MODEL_FILE: &str = "model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

/// 「算装好了」的最小体积：任何 BGE 变体的 ONNX 都是几十 MB 起，tokenizer.json
/// 也有数百 KB。取值远低于真实大小、又足以挡住 0 字节/截断文件（见
/// [`bge_model_ready`] 的说明）。
const MODEL_MIN_BYTES: u64 = 1_000_000;
const TOKENIZER_MIN_BYTES: u64 = 1_000;

/// (model_name, display_name, dim)
const LOCAL_MODELS: &[(&str, &str, u32)] = &[
    ("bge-large-zh-v1.5", "BGE-Large (1024维)", 1024),
    ("bge-base-zh-v1.5", "BGE-Base (768维)", 768),
    ("bge-small-zh-v1.5", "BGE-Small (512维, 快)", 512),
];

/// Output dimension of a known built-in local model, or `None` when unknown.
/// Used by the embedding-consistency check to detect stale vectors after a
/// model switch.
pub fn local_model_dim(model_name: &str) -> Option<u32> {
    LOCAL_MODELS
        .iter()
        .find(|(name, _, _)| *name == model_name)
        .map(|(_, _, dim)| *dim)
}

/// Remove every built-in BGE model directory except `keep`, so at most one
/// dimension lives on disk. Best-effort: a locked/in-use directory is logged
/// and skipped rather than failing the caller.
pub fn prune_local_models_except(data_dir: &Path, keep: &str) {
    for &(name, _, _) in LOCAL_MODELS {
        if name == keep {
            continue;
        }
        let dir = data_dir.join("models").join(name);
        if !dir.exists() {
            continue;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => log::info!("[BGE] 已删除其它维度模型: {}", dir.display()),
            Err(e) => log::warn!("[BGE] 删除模型目录失败 {}: {e}", dir.display()),
        }
    }
}

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
    /// Local model directory name (e.g. `bge-large-zh-v1.5`) — the argument
    /// `install_bge` expects and the suffix of `local:<model_id>`.
    pub model_id: String,
}

/// 安装标志的 RAII 复位：下载线程 panic 也要复位，
/// 否则"已在后台进行中"会永久残留（此后所有安装请求都被拒绝，直到重启）。
struct InstallingGuard;
impl Drop for InstallingGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}

#[tauri::command]
pub fn install_bge(state: State<'_, AppState>, app: AppHandle, model_name: Option<String>) -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("BGE 模型下载已在后台进行中".into());
    }

    let name = model_name.unwrap_or_else(|| {
        let cfg = crate::config::load_config();
        crate::ai::local_embed::local_model_dir_name(&cfg.active_embedding_model_id)
            .unwrap_or("bge-large-zh-v1.5")
            .to_string()
    });
    let model_dir = state.data_dir.join("models").join(&name);
    // 早退必须复位 INSTALLING，否则这个标志永远为 true（此前就是这里
    // `?` 直接返回：建目录失败一次，之后所有重试都报"已在后台进行中"）。
    if let Err(e) = std::fs::create_dir_all(&model_dir) {
        INSTALLING.store(false, Ordering::SeqCst);
        return Err(format!("创建目录失败: {e}"));
    }

    let data_dir = state.data_dir.clone();
    std::thread::spawn(move || run_download(model_dir, name, data_dir, app));
    Ok(())
}

fn run_download(model_dir: PathBuf, model_name: String, data_dir: PathBuf, app: AppHandle) {
    let _reset = InstallingGuard;
    let result = download_inner(&model_dir, &model_name);
    // A successful install prunes every other dimension so only one local
    // model is kept (the settings UI is a 3-way choice).
    if result.success {
        prune_local_models_except(&data_dir, &model_name);
    }
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
            model_id: name.to_string(),
        });
    }
    Ok(statuses)
}

pub fn bge_model_ready(model_dir: &Path) -> bool {
    // ⚠️ 不能只看 `is_file()`：崩溃/断电留下的**半截文件**也满足它，于是模型被
    // 当成"已安装"，`download_inner` 又因为 `dest.is_file()` 直接跳过 —— 坏模型
    // 永久不可恢复（推理时才报错，用户还不明白为什么重装没用）。
    // 这里加最小体积门限，配合 `download()` 的 `.part` + 原子改名，两道一起堵。
    let big_enough = |name: &str, min: u64| {
        std::fs::metadata(model_dir.join(name))
            .map(|m| m.is_file() && m.len() >= min)
            .unwrap_or(false)
    };
    big_enough(MODEL_FILE, MODEL_MIN_BYTES) && big_enough(TOKENIZER_FILE, TOKENIZER_MIN_BYTES)
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
        // 只认「够大」的现有文件：半截文件（旧版本直接从网络写最终文件名留下的）
        // 必须重新下载，否则 `bge_model_ready` 永远为假、而这里永远跳过 —— 用户
        // 重装一百次也没用。
        let min = if *local == MODEL_FILE { MODEL_MIN_BYTES } else { TOKENIZER_MIN_BYTES };
        let usable = std::fs::metadata(&dest)
            .map(|m| m.is_file() && m.len() >= min)
            .unwrap_or(false);
        if usable {
            log::info!("[BGE] {local} 已存在，跳过");
            continue;
        }
        if dest.exists() {
            log::warn!("[BGE] {local} 体积异常（疑似半截文件），重新下载");
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
    // **先写 `<name>.part`，成功后才原子改名**：中途断网/进程被杀留下的半成品
    // 永远不叫 `model.onnx`，不会被下次安装当作"已存在"跳过。
    let part = crate::deps::download::part_path_for(dest);
    let _ = std::fs::remove_file(&part);
    let mut file = File::create(&part).map_err(|e| format!("{e}"))?;
    let mut buf = [0u8; 64 * 1024];
    let mut written = 0u64;
    let mut last_pct = 0u32;
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > PER_SOURCE_TIMEOUT {
            let _ = std::fs::remove_file(&part);
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
    // 完整性：声明了长度就必须一字不差地拿到（截断的连接也可能以 EOF 正常结束，
    // 以前会把它当成功）。长度未知（chunked）时只能靠上面的超时兜底。
    if total > 0 && written != total {
        let _ = std::fs::remove_file(&part);
        return Err(format!("下载不完整（{written}/{total} 字节）"));
    }
    file.flush().map_err(|e| format!("{e}"))?;
    drop(file); // Windows 上先关句柄再改名，避免共享冲突
    std::fs::rename(&part, dest).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("重命名失败: {e}")
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 半截文件（旧版本直接写最终文件名的遗留）必须**不算**就绪：否则
    /// `download_inner` 跳过"已存在"的文件、`bge_model_ready` 又永远为假，
    /// 模型永久不可恢复（重装多少次都没用）。
    #[test]
    fn bge_model_ready_requires_plausible_size() {
        let dir = std::env::temp_dir().join(format!("ls_bge_ready_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 空文件 → 未就绪
        std::fs::write(dir.join(MODEL_FILE), b"").unwrap();
        std::fs::write(dir.join(TOKENIZER_FILE), b"").unwrap();
        assert!(!bge_model_ready(&dir), "空文件不算就绪");

        // 截断（比门限少 1 字节）→ 未就绪
        std::fs::write(dir.join(MODEL_FILE), vec![0u8; MODEL_MIN_BYTES as usize - 1]).unwrap();
        std::fs::write(dir.join(TOKENIZER_FILE), vec![0u8; TOKENIZER_MIN_BYTES as usize - 1])
            .unwrap();
        assert!(!bge_model_ready(&dir), "截断文件不算就绪");

        // 达到门限 → 就绪
        std::fs::write(dir.join(MODEL_FILE), vec![0u8; MODEL_MIN_BYTES as usize]).unwrap();
        std::fs::write(dir.join(TOKENIZER_FILE), vec![0u8; TOKENIZER_MIN_BYTES as usize]).unwrap();
        assert!(bge_model_ready(&dir), "达到门限即就绪");

        // 缺一个文件 → 未就绪
        std::fs::remove_file(dir.join(TOKENIZER_FILE)).unwrap();
        assert!(!bge_model_ready(&dir), "缺文件不算就绪");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
