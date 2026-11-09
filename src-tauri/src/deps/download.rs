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

use sha2::{Digest, Sha256};

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
    // `remote` may be an absolute URL: the diarization models live in two
    // different upstream repos, so one `<base>/<remote>` prefix cannot serve
    // both files.
    let canonical = if remote.starts_with("http://") || remote.starts_with("https://") {
        remote.to_string()
    } else {
        format!(
            "{}/{}",
            source.base_url.trim_end_matches('/'),
            remote
        )
    };

    // HuggingFace：`hf-mirror.com`（国内快）与 `huggingface.co`（官方）互为兜底。
    // 实测 hf-mirror 会返回 429 限流，只有镜像是不够的。
    if canonical.contains("://hf-mirror.com/") {
        return vec![
            canonical.clone(),
            canonical.replace("://hf-mirror.com/", "://huggingface.co/"),
        ];
    }
    if canonical.contains("://huggingface.co/") {
        return vec![
            canonical.replace("://huggingface.co/", "://hf-mirror.com/"),
            canonical,
        ];
    }

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

/// 下载完成后的二次加工：有些上游只提供"打包形态"，落地前要取成员 / 抽内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostStep {
    /// 下载的是 zip，取出文件名以该后缀结尾的成员写到 `local`
    /// （例：`lib/onnxruntime.dll`）。
    ZipMember(String),
    /// 下载的是 PaddleOCR 的 `inference.yml`，把 `character_dict` 抽成字典文件。
    YamlDict,
}

/// 一个待下载文件。
#[derive(Debug, Clone)]
pub struct DownloadFile {
    /// Releases 资产名或**绝对 URL**（上游直连）。
    pub remote: String,
    /// 落地文件名（可含子目录）。
    pub local: String,
    /// 被下载**原始文件**的 sha256（不是加工后的结果）。
    pub sha256: String,
    pub post: Option<PostStep>,
}

impl DownloadFile {
    pub fn plain(remote: impl Into<String>, local: impl Into<String>, sha256: impl Into<String>) -> Self {
        Self { remote: remote.into(), local: local.into(), sha256: sha256.into(), post: None }
    }
}

/// Install a dep's files into `data_dir`（老布局：平铺到 `install_dir`）。
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
    let files: Vec<DownloadFile> = def
        .files
        .iter()
        .map(|f| DownloadFile {
            remote: f.remote.to_string(),
            local: f.local.to_string(),
            sha256: f.sha256.to_string(),
            post: crate::deps::catalog::post_step_for(def.id, f),
        })
        .collect();
    install_into(&files, &def.sources, &dest_dir, def.id, def.size_bytes, cancel, on_progress)
}

/// Install an explicit file list into `dest_dir`（版本化目录 / 平铺目录共用）。
///
/// Idempotent — already-present files verified against their SHA-256 are
/// skipped; a truncated/interrupted download resumes from its `.part`.
pub fn install_into(
    files: &[DownloadFile],
    sources_in: &[Source],
    dest_dir: &Path,
    label: &str,
    total_bytes_hint: u64,
    cancel: &AtomicBool,
    on_progress: &ProgressFn,
) -> Result<(), DownloadError> {
    if files.is_empty() {
        return err(format!("依赖 '{label}' 没有可下载文件"));
    }

    fs::create_dir_all(dest_dir)
        .map_err(|e| DownloadError(format!("创建目录失败: {e}")))?;

    // Files that are missing OR fail their integrity check need (re)fetching.
    // `files_ready` only checks existence/non-empty; here we do the real
    // SHA-256 verification so a corrupt file from a previous crash is redone.
    let mut pending: Vec<&DownloadFile> = Vec::new();
    for f in files {
        let local_path = dest_dir.join(&f.local);
        // 加工产物（解压 zip / 抽字典）与"原始包"的 sha256 不同，无法直接比对；
        // 下载阶段已经校验过原始包，这里存在且非空即视为完成。
        if f.post.is_some() {
            if local_path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                continue;
            }
            pending.push(f);
            continue;
        }
        if local_path.is_file() {
            match file_sha256(&local_path) {
                Ok(digest) if digest == f.sha256 => continue, // verified, skip
                Ok(digest) => {
                    log::warn!(
                        "[DEPS] {} 已存在但校验失败 (got {digest}, want {}), 重新下载",
                        f.local,
                        f.sha256
                    );
                    let _ = fs::remove_file(&local_path);
                }
                Err(e) => {
                    log::warn!("[DEPS] {} 读取校验失败: {e}, 重新下载", f.local);
                    let _ = fs::remove_file(&local_path);
                }
            }
        }
        pending.push(f);
    }

    if pending.is_empty() {
        return Ok(());
    }

    log::info!(
        "[DEPS] {}: {} file(s) to fetch (~{:.1} MB total)",
        label,
        pending.len(),
        total_bytes_hint as f64 / 1_048_576.0
    );

    let total_files = pending.len() as u64;
    let mut done_files = 0u64;

    for file in &pending {
        if cancel.load(Ordering::SeqCst) {
            return err("已取消");
        }
        let local_path = dest_dir.join(&file.local);
        // Nested local paths (e.g. `Qwen3-0.6B/tokenizer.json`) need their
        // parent dir created before the final rename.
        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| DownloadError(format!("创建目录失败: {e}")))?;
        }
        let part_path = part_path_for(&local_path);

        let mut last_error: Option<String> = None;
        let mut succeeded = false;

        // Expand every catalog source into concrete URLs (mirror first for
        // GitHub) and try them in order. Each source resumes from the bytes
        // already written by the previous one (`.part` is never deleted on a
        // failed source so the next one can pick up where it stopped).
        let mut url_plan: Vec<(String, String)> = Vec::new();
        // Deps whose files are all absolute URLs declare no sources; give them
        // one synthetic source so `effective_urls` still yields the URL.
        let fallback_source = Source { label: "上游", base_url: String::new() };
        let sources: Vec<Source> = if sources_in.is_empty() {
            vec![fallback_source]
        } else {
            sources_in.to_vec()
        };
        for source in &sources {
            for url in effective_urls(source, &file.remote) {
                url_plan.push((source.label.to_string(), url));
            }
        }

        for (label, url) in &url_plan {
            if cancel.load(Ordering::SeqCst) {
                return err("已取消");
            }
            let offset = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
            log::info!(
                "[DEPS] {} ← {} ({}){}",
                file.local,
                label,
                url,
                if offset > 0 {
                    format!(", 断点续传 @ {offset}B")
                } else {
                    String::new()
                }
            );

            match download_one(url, &part_path, offset, cancel, &mut |written, _total| {
                on_progress(done_files + 1, total_files, written);
            }) {
                Ok(n) => {
                    if n == 0 && offset == 0 {
                        last_error = Some(format!("空文件（源 {label}）"));
                        let _ = fs::remove_file(&part_path);
                        continue;
                    }
                    // Verify integrity before atomically renaming into place.
                    match file_sha256(&part_path) {
                        Ok(digest) if digest == file.sha256 => {
                            match &file.post {
                                Some(step) => {
                                    materialize(step, &part_path, &local_path)?;
                                    let _ = fs::remove_file(&part_path);
                                }
                                None => {
                                    fs::rename(&part_path, &local_path).map_err(|e| {
                                        DownloadError(format!(
                                            "写入 {} 失败: {e}",
                                            local_path.display()
                                        ))
                                    })?;
                                }
                            }
                            succeeded = true;
                            log::info!(
                                "[DEPS] {} 完成 ({:.1} MB, sha256 ✓)",
                                file.local,
                                n as f64 / 1_048_576.0
                            );
                            break;
                        }
                        Ok(digest) => {
                            // Corrupt download (or an uncooperative source that
                            // ignored Range and restarted). Drop it and move on.
                            let _ = fs::remove_file(&part_path);
                            last_error = Some(format!(
                                "sha256 校验失败（源 {label}, got {digest}）"
                            ));
                            continue;
                        }
                        Err(e) => {
                            let _ = fs::remove_file(&part_path);
                            last_error = Some(format!("校验读取失败（源 {label}）: {e}"));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    // Keep the `.part` so the next source (or a later retry of
                    // the whole install) can resume instead of restarting.
                    last_error = Some(format!("{}（源 {label}）", e.0));
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

/// Compute the lowercase hex SHA-256 of a file.
/// 把下载好的"原始包"加工成目标文件（解压 / 抽字典）。
fn materialize(step: &PostStep, src: &Path, dest: &Path) -> Result<(), DownloadError> {
    match step {
        PostStep::ZipMember(suffix) => extract_zip_member(src, suffix, dest),
        PostStep::YamlDict => yaml_dict_to_file(src, dest).map(|_| ()),
    }
}

/// 从 zip 里取出文件名以 `suffix` 结尾的成员写到 `dest`。
///
/// 上游的 ORT 压缩包路径带版本号（`onnxruntime-win-x64-1.31.1/lib/onnxruntime.dll`），
/// 所以按**后缀**匹配，升级时不用改代码。
fn extract_zip_member(zip_path: &Path, suffix: &str, dest: &Path) -> Result<(), DownloadError> {
    let f = fs::File::open(zip_path)
        .map_err(|e| DownloadError(format!("打开压缩包失败: {e}")))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| DownloadError(format!("解析压缩包失败: {e}")))?;

    let mut target: Option<String> = None;
    for name in zip.file_names() {
        if name.ends_with(suffix) {
            target = Some(name.to_string());
            break;
        }
    }
    let Some(target) = target else {
        return err(format!("压缩包里找不到 *{suffix}"));
    };

    let mut member = zip
        .by_name(&target)
        .map_err(|e| DownloadError(format!("读取 {target} 失败: {e}")))?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| DownloadError(format!("创建目录失败: {e}")))?;
    }
    let mut out = fs::File::create(dest)
        .map_err(|e| DownloadError(format!("写入 {} 失败: {e}", dest.display())))?;
    std::io::copy(&mut member, &mut out)
        .map_err(|e| DownloadError(format!("解压 {target} 失败: {e}")))?;
    log::info!("[DEPS] 已从压缩包取出 {target} -> {}", dest.display());
    Ok(())
}

/// 从 PaddleOCR 的 `inference.yml` 里抽 `character_dict` 写成字典文件。
///
/// 只认 `character_dict:` 之后的 `- <标量>` 行（这是该导出格式的固定写法），
/// 遇到别的键就停；条目数 < 1000 视为解析失败（宁可报错也不装一份坏字典）。
fn yaml_dict_to_file(yml: &Path, dest: &Path) -> Result<usize, DownloadError> {
    let text = fs::read_to_string(yml)
        .map_err(|e| DownloadError(format!("读取 inference.yml 失败: {e}")))?;
    let mut chars: Vec<String> = Vec::new();
    let mut in_dict = false;
    for line in text.lines() {
        if !in_dict {
            if line.trim_start().starts_with("character_dict:") {
                in_dict = true;
            }
            continue;
        }
        let trimmed_start = line.trim_start();
        if let Some(rest) = trimmed_start.strip_prefix("- ") {
            chars.push(unquote_scalar(rest));
        } else if trimmed_start == "-" {
            // `- ` 后面恰好是空白字符（例如 U+3000）时会被 trim 掉
            chars.push(String::new());
        } else if trimmed_start.is_empty() {
            continue;
        } else {
            break;
        }
    }
    if chars.len() < 1000 {
        return err(format!("字典条目异常（{} 条），拒绝写入", chars.len()));
    }
    let mut body = String::with_capacity(chars.len() * 4);
    for c in &chars {
        body.push_str(c);
        body.push('\n');
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| DownloadError(format!("创建目录失败: {e}")))?;
    }
    fs::write(dest, body)
        .map_err(|e| DownloadError(format!("写入字典 {} 失败: {e}", dest.display())))?;
    Ok(chars.len())
}

fn unquote_scalar(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace("''", "'")
    } else if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\")
    } else {
        s.to_string()
    }
}

pub(crate) fn file_sha256(path: &Path) -> Result<String, DownloadError> {
    let mut file = fs::File::open(path)
        .map_err(|e| DownloadError(format!("打开失败: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; CHUNK];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| DownloadError(format!("读取失败: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Download one URL into `dest` (a `.part` path) starting at `resume_from`
/// bytes already present, returning total bytes in the file afterwards.
/// Appends when the server honors `Range` (HTTP 206); restarts from zero when
/// the server ignores it (HTTP 200). If the server answers 416 (Range Not
/// Satisfiable — e.g. the file on the server changed and is now smaller than
/// our partial copy), the stale `.part` is discarded and the download restarts
/// from zero once.
fn download_one(
    url: &str,
    dest: &Path,
    resume_from: u64,
    cancel: &AtomicBool,
    on_chunk: &mut dyn FnMut(u64, u64),
) -> Result<u64, DownloadError> {
    let client = ureq::builder()
        .try_proxy_from_env(true)
        .timeout(PER_SOURCE_TIMEOUT)
        .build();

    // First attempt; on 416 with a stale partial file, retry from zero.
    for attempt in 0..2 {
        let offset = if attempt == 0 { resume_from } else { 0 };
        if attempt == 1 {
            // Source no longer satisfies our Range — our partial copy is stale.
            log::warn!("[DEPS] 源返回 416 (Range 不满足)，丢弃部分文件从头下载");
            let _ = fs::remove_file(dest);
        }

        let mut req = client.get(url);
        if offset > 0 {
            req = req.set("Range", &format!("bytes={offset}-"));
        }
        match req.call() {
            Err(ureq::Error::Status(416, _)) if offset > 0 => continue,
            Err(e) => return Err(DownloadError(format!("连接失败: {e}"))),
            Ok(resp) => {
                let status = resp.status();
                let (append, total, base) = if status == 206 {
                    // Resuming: Content-Range has `bytes <start>-<end>/<total>`.
                    let start = resp
                        .header("Content-Range")
                        .and_then(|v| v.split('/').next())
                        .and_then(|v| v.split('-').next())
                        .and_then(|v| v.trim_start_matches("bytes ").parse::<u64>().ok())
                        .unwrap_or(offset);
                    let total = resp
                        .header("Content-Range")
                        .and_then(|v| v.rsplit('/').next())
                        .and_then(|v| v.parse::<u64>().ok())
                        .or_else(|| resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()))
                        .unwrap_or(0);
                    (true, total, start)
                } else {
                    // 200 (or anything else) = full body from the start.
                    let total = resp
                        .header("Content-Length")
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0);
                    (false, total, 0)
                };

                let mut file = if append {
                    fs::OpenOptions::new()
                        .append(true)
                        .open(dest)
                        .map_err(|e| DownloadError(format!("打开续传文件失败: {e}")))?
                } else {
                    fs::File::create(dest).map_err(|e| DownloadError(format!("创建文件失败: {e}")))?
                };

                let mut reader = resp.into_reader();
                let mut buf = [0u8; CHUNK];
                let mut written: u64 = base;
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

                return Ok(written);
            }
        }
    }
    // Unreachable in practice: the loop either returns or `continue`s at most
    // once. Guarded for the compiler.
    Err(DownloadError("下载失败".to_string()))
}

/// `<name>.part` 兄弟路径。模型下载（BGE 等）也复用同一命名约定：
/// 先写 `.part`、成功后原子改名 —— 中途被杀/断网留下的半成品永远不会被当成
/// 完整文件。见 `commands::bge::download`。
pub(crate) fn part_path_for(path: &Path) -> PathBuf {
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

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ls_dl_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// 上游直连：HF 镜像与官方互为兜底（hf-mirror 实测会 429）。
    #[test]
    fn huggingface_urls_fall_back_between_mirror_and_canonical() {
        let src = Source { label: "upstream".into(), base_url: String::new() };
        let mirror = "https://hf-mirror.com/PaddlePaddle/X/resolve/abc/inference.onnx";
        let urls = effective_urls(&src, mirror);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].contains("hf-mirror.com"));
        assert!(urls[1].contains("huggingface.co"));

        let canonical = "https://huggingface.co/PaddlePaddle/X/resolve/abc/inference.onnx";
        let urls = effective_urls(&src, canonical);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].contains("hf-mirror.com"), "国内优先镜像: {urls:?}");
        assert!(urls[1].contains("huggingface.co"));
    }

    /// 上游 ORT 包是 zip：按后缀取出 `lib/onnxruntime.dll`。
    #[test]
    fn extracts_zip_member_by_suffix() {
        use std::io::Write as _;
        let dir = tmp_dir("zip");
        let zip_path = dir.join("onnxruntime.zip");
        {
            let f = fs::File::create(&zip_path).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            z.start_file("onnxruntime-win-x64-1.31.1/include/onnxruntime_cxx_api.h", opts)
                .unwrap();
            z.write_all(b"header").unwrap();
            z.start_file("onnxruntime-win-x64-1.31.1/lib/onnxruntime.dll", opts)
                .unwrap();
            z.write_all(b"DUMMY-DLL").unwrap();
            z.finish().unwrap();
        }
        let dest = dir.join("runtime").join("onnxruntime.dll");
        extract_zip_member(&zip_path, "lib/onnxruntime.dll", &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"DUMMY-DLL");
        let _ = fs::remove_dir_all(&dir);
    }

    /// PaddleOCR 的字典藏在 `inference.yml` 里，按 `character_dict:` 段抽取。
    #[test]
    fn yaml_dict_extracts_character_list() {
        let dir = tmp_dir("yaml");
        let yml = dir.join("inference.yml");
        let mut body = String::from("Global:\n  model_name: x\nPostProcess:\n  character_dict:\n");
        for i in 0..1200 {
            body.push_str(&format!("  - c{i}\n"));
        }
        // 引号与空白字符（U+3000）都要原样保留
        body.push_str("  - ''''\n  - 　\n");
        body.push_str("NextSection:\n  k: v\n");
        fs::write(&yml, body).unwrap();

        let out = dir.join("dict.txt");
        let n = yaml_dict_to_file(&yml, &out).unwrap();
        assert_eq!(n, 1202);
        let text = fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "c0");
        assert_eq!(*lines.last().unwrap(), "\u{3000}");
        assert!(lines.contains(&"'"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// 字典条目太少必须拒绝（防止把半个 yml 当字典装进去）。
    #[test]
    fn yaml_dict_rejects_tiny_dicts() {
        let dir = tmp_dir("yaml_bad");
        let yml = dir.join("inference.yml");
        fs::write(&yml, "PostProcess:\n  character_dict:\n  - a\n  - b\n").unwrap();
        let out = dir.join("dict.txt");
        assert!(yaml_dict_to_file(&yml, &out).is_err());
        assert!(!out.exists());
        let _ = fs::remove_dir_all(&dir);
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
        // HF 现在会给出「镜像 + 官方」两条（实测 hf-mirror 会 429）
        assert_eq!(
            urls,
            vec![
                "https://hf-mirror.com/owner/resolve/main/model.onnx",
                "https://huggingface.co/owner/resolve/main/model.onnx",
            ]
        );
    }

    /// 说话人分离模型来自两个不同上游仓库 → `remote` 必须是绝对 URL，
    /// 且不能被 `base_url` 再拼一次。
    #[test]
    fn absolute_remote_url_bypasses_base_url() {
        let guard = lock_env();
        clear_env(&guard);
        let source = Source { label: "上游", base_url: String::new() };
        let urls = effective_urls(
            &source,
            "https://hf-mirror.com/csukuangfj/pyannote/resolve/main/model.onnx",
        );
        assert_eq!(
            urls,
            vec![
                "https://hf-mirror.com/csukuangfj/pyannote/resolve/main/model.onnx",
                "https://huggingface.co/csukuangfj/pyannote/resolve/main/model.onnx",
            ]
        );

        // 绝对 GitHub URL 仍享受镜像链（国内直连 ~29KB/s 实测）。
        let gh = effective_urls(
            &source,
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/x/emb.onnx",
        );
        assert_eq!(gh.len(), 3);
        assert!(gh[0].starts_with("https://ghfast.top/"));
        assert!(gh.last().unwrap().starts_with("https://github.com/"));
    }

    #[test]
    fn file_sha256_matches_known_digest() {
        // sha256("hello") — checks both computation and hex formatting.
        let dir = std::env::temp_dir().join(format!("ls_dl_sha_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hello.txt");
        std::fs::write(&p, b"hello").unwrap();
        let digest = super::file_sha256(&p).unwrap();
        assert_eq!(
            digest,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_sha256_errors_on_missing() {
        let digest = super::file_sha256(Path::new("/nonexistent/ls_definitely_missing.bin"));
        assert!(digest.is_err());
    }
}
