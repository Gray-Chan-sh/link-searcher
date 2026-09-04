//! Mirror-first downloader used by the dependency installer.
//!
//! - Streams each file to a `*.part` sibling, then atomically renames it into
//!   place on success — a crash/interrupt never leaves a half file that is
//!   mistaken for a complete download.
//! - GitHub Releases are fetched through a chain of China-friendly mirrors
//!   first (ghproxy-style) and fall back to the canonical URL only when every
//!   mirror fails. Set `LINK_SEARCHER_NO_MIRROR=1` to skip mirrors, or
//!   `LINK_SEARCHER_GH_MIRROR` to prepend your own prefix to the chain.
//! - Reports progress via a callback, which the command layer forwards to the
//!   frontend as `dep-progress` events.
//! - Cooperative cancellation via an `AtomicBool`.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::deps::catalog::{DepDef, Source};

/// Progress callback: `(current_file_index_1based, total_files, current_file_bytes)`.
pub type ProgressFn = dyn Fn(u64, u64, u64) + Send + Sync;

const CHUNK: usize = 64 * 1024;
const PER_SOURCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Default China-friendly GitHub release mirrors, fastest first. Each is a
/// prefix that proxies `https://github.com/<owner>/<repo>/releases/download/...`.
/// The canonical GitHub URL is always tried last. Measured 2026-09: ghfast.top
/// ≈4.3 MB/s, gh-proxy.com ≈3.7 MB/s vs ~29 KB/s direct from CN networks.
const DEFAULT_GH_MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
];

/// Expand a catalog source into the concrete URL list tried in order:
/// user-picked mirror (if set), the default mirror chain, then the canonical
/// GitHub URL.
fn effective_urls(source: &Source, remote: &str) -> Vec<String> {
    let canonical = format!(
        "{}/{}",
        source.base_url.trim_end_matches('/'),
        remote
    );
    if !canonical.starts_with("https://github.com/") {
        return vec![canonical];
    }
    if std::env::var("LINK_SEARCHER_NO_MIRROR").as_deref() == Ok("1") {
        return vec![canonical];
    }
    // User override goes first (fastest path when set), then the defaults.
    let mut mirrors: Vec<String> = Vec::with_capacity(DEFAULT_GH_MIRRORS.len() + 1);
    if let Ok(user_mirror) = std::env::var("LINK_SEARCHER_GH_MIRROR") {
        if !user_mirror.trim().is_empty() {
            mirrors.push(user_mirror.trim_end_matches('/').to_string());
        }
    }
    mirrors.extend(DEFAULT_GH_MIRRORS.iter().map(|m| m.trim_end_matches('/').to_string()));

    let mut urls = Vec::with_capacity(mirrors.len() + 1);
    for prefix in &mirrors {
        urls.push(format!("{prefix}/{canonical}"));
    }
    urls.push(canonical);
    urls
}

#[derive(Debug, Clone)]
pub struct DownloadError(pub String);

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DownloadError {}

fn err<T>(msg: impl Into<String>) -> Result<T, DownloadError> {
    Err(DownloadError(msg.into()))
}

/// Install a dep's files into `data_dir`. Idempotent — already-present files
/// are skipped. `on_progress(current_file, total_files, bytes_downloaded_of_current)`
pub fn install_dep(
    def: &DepDef,
    data_dir: &Path,
    cancel: &AtomicBool,
    on_progress: &ProgressFn,
) -> Result<(), DownloadError> {
    if def.files.is_empty() {
        return err(format!("dep '{}' has no downloadable files", def.id));
    }

    let dest_dir = crate::deps::catalog::install_dir(def, data_dir);
    fs::create_dir_all(&dest_dir)
        .map_err(|e| DownloadError(format!("创建目录失败: {e}")))?;

    // Skip files already present (idempotent resume across retries).
    let pending: Vec<_> = def
        .files
        .iter()
        .filter(|f| !dest_dir.join(f.local).is_file())
        .collect();

    if pending.is_empty() {
        return Ok(());
    }

    log::info!(
        "[DEPS] {}: {} file(s) to fetch (~{:.1} MB total)",
        def.id,
        pending.len(),
        def.size_bytes as f64 / 1_048_576.0
    );

    let total_files = pending.len() as u64;
    let mut done_files = 0u64;

    for file in &pending {
        if cancel.load(Ordering::SeqCst) {
            return err("已取消");
        }
        let local_path = dest_dir.join(file.local);
        // Nested local paths (e.g. `Qwen3-0.6B/tokenizer.json`) need their
        // parent dir created before the final rename.
        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| DownloadError(format!("创建目录失败: {e}")))?;
        }
        let part_path = part_path_for(&local_path);
        let _ = fs::remove_file(&part_path);

        let mut last_error: Option<String> = None;
        let mut succeeded = false;

        // Expand every catalog source into concrete URLs (mirror first for
        // GitHub) and try them in order.
        let mut url_plan: Vec<(String, String)> = Vec::new();
        for source in &def.sources {
            for url in effective_urls(source, file.remote) {
                url_plan.push((source.label.to_string(), url));
            }
        }

        for (label, url) in &url_plan {
            if cancel.load(Ordering::SeqCst) {
                return err("已取消");
            }
            log::info!("[DEPS] {} ← {} ({})", file.local, label, url);

            match download_one(url, &part_path, cancel, &mut |written, _total| {
                on_progress(done_files + 1, total_files, written);
            }) {
                Ok(n) => {
                    if n == 0 {
                        last_error = Some(format!("空文件（源 {label}）"));
                        let _ = fs::remove_file(&part_path);
                        continue;
                    }
                    fs::rename(&part_path, &local_path).map_err(|e| {
                        DownloadError(format!("写入 {} 失败: {e}", local_path.display()))
                    })?;
                    succeeded = true;
                    log::info!(
                        "[DEPS] {} 完成 ({:.1} MB)",
                        file.local,
                        n as f64 / 1_048_576.0
                    );
                    break;
                }
                Err(e) => {
                    last_error = Some(format!("{}（源 {label}）", e.0));
                    let _ = fs::remove_file(&part_path);
                }
            }
        }

        if !succeeded {
            return err(format!(
                "下载 {} 失败：所有镜像源均不可用。{}",
                file.remote,
                last_error.unwrap_or_else(|| "未知错误".into())
            ));
        }
        done_files += 1;
    }

    Ok(())
}

/// Download one URL into `dest` (a `.part` path), returning bytes written.
fn download_one(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    on_chunk: &mut dyn FnMut(u64, u64),
) -> Result<u64, DownloadError> {
    let resp = ureq::builder()
        .try_proxy_from_env(true)
        .timeout(PER_SOURCE_TIMEOUT)
        .build()
        .get(url)
        .call()
        .map_err(|e| DownloadError(format!("连接失败: {e}")))?;

    let total = resp
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let mut reader = resp.into_reader();
    let mut file =
        fs::File::create(dest).map_err(|e| DownloadError(format!("创建文件失败: {e}")))?;
    let mut buf = [0u8; CHUNK];
    let mut written: u64 = 0;
    let started = std::time::Instant::now();

    loop {
        if cancel.load(Ordering::SeqCst) {
            return err("已取消");
        }
        if started.elapsed() > PER_SOURCE_TIMEOUT {
            return err(format!("下载超时（{}s）", PER_SOURCE_TIMEOUT.as_secs()));
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| DownloadError(format!("读取失败: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| DownloadError(format!("写入失败: {e}")))?;
        written += n as u64;
        on_chunk(written, total);
    }
    file.sync_all()
        .map_err(|e| DownloadError(format!("同步失败: {e}")))?;
    drop(file);

    Ok(written)
}

fn part_path_for(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(".part");
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    // `effective_urls` reads process-global env vars; serialize the tests so
    // concurrent runs can't race each other's LINK_SEARCHER_* overrides.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn clear_env(_guard: &MutexGuard<'static, ()>) {
        // edition 2024 marks env mutation unsafe.
        unsafe {
            std::env::remove_var("LINK_SEARCHER_GH_MIRROR");
            std::env::remove_var("LINK_SEARCHER_NO_MIRROR");
        }
    }

    fn gh_source() -> Source {
        Source {
            label: "GitHub Releases",
            base_url: "https://github.com/owner/repo/releases/download/tag".to_string(),
        }
    }

    #[test]
    fn github_urls_try_mirror_chain_then_canonical() {
        let guard = lock_env();
        clear_env(&guard);
        let urls = effective_urls(&gh_source(), "file.onnx");
        assert_eq!(
            urls,
            vec![
                "https://ghfast.top/https://github.com/owner/repo/releases/download/tag/file.onnx",
                "https://gh-proxy.com/https://github.com/owner/repo/releases/download/tag/file.onnx",
                "https://github.com/owner/repo/releases/download/tag/file.onnx",
            ]
        );
    }

    #[test]
    fn user_mirror_is_prepended() {
        let guard = lock_env();
        clear_env(&guard);
        unsafe {
            std::env::set_var("LINK_SEARCHER_GH_MIRROR", "https://my.mirror/");
        }
        let urls = effective_urls(&gh_source(), "file.onnx");
        assert!(
            urls[0].starts_with("https://my.mirror/"),
            "user mirror should be first, got {urls:?}"
        );
        assert_eq!(urls.len(), 4);
    }

    #[test]
    fn no_mirror_skips_chain() {
        let guard = lock_env();
        clear_env(&guard);
        unsafe {
            std::env::set_var("LINK_SEARCHER_NO_MIRROR", "1");
        }
        let urls = effective_urls(&gh_source(), "file.onnx");
        assert_eq!(urls, vec!["https://github.com/owner/repo/releases/download/tag/file.onnx"]);
    }

    #[test]
    fn non_github_source_uses_canonical_only() {
        let guard = lock_env();
        clear_env(&guard);
        let source = Source {
            label: "HF",
            base_url: "https://huggingface.co/owner/resolve/main".to_string(),
        };
        let urls = effective_urls(&source, "model.onnx");
        assert_eq!(urls, vec!["https://huggingface.co/owner/resolve/main/model.onnx"]);
    }
}
