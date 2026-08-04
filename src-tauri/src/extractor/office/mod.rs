use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use calamine::Reader as CalamineReader;
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;

use super::Extractor;
use crate::config::load_config;
use crate::scanner::helpers::TempDir;

pub fn is_libreoffice_available() -> bool {
    if let Ok(lo_bin) = std::env::var("LO_BINARY") {
        if !lo_bin.is_empty() && check_binary(&lo_bin) {
            return true;
        }
    }
    let config = load_config();
    if !config.lo_binary_path.is_empty() && check_binary(&config.lo_binary_path) {
        return true;
    }
    for path in common_lo_paths() {
        if check_binary(&path) {
            return true;
        }
    }
    check_binary("soffice")
}

fn check_binary(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    // Only check file existence for absolute/relative paths, not for bare commands (like "soffice") that rely on PATH resolution
    let is_bare_name = !path.contains('/') && !path.contains('\\');
    if !is_bare_name && !Path::new(path).exists() {
        return false;
    }
    // First-run profile creation can take a while, so don't block forever.
    let mut child = match std::process::Command::new(path)
        .arg("--headless")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match wait_timeout(&mut child, Duration::from_secs(15)) {
        Ok(Some(status)) => status.success(),
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            false
        }
        Err(_) => false,
    }
}

fn wait_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(Some(status)),
            None => {
                if start.elapsed() >= timeout {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn common_lo_paths() -> Vec<String> {
    let mut paths = Vec::new();
    if cfg!(target_os = "macos") {
        paths.push("/opt/homebrew/bin/soffice".into());
        paths.push("/usr/local/bin/soffice".into());
        paths.push("/Applications/LibreOffice.app/Contents/MacOS/soffice".into());
    } else if cfg!(target_os = "windows") {
        paths.push("C:\\Program Files\\LibreOffice\\program\\soffice.exe".into());
        paths.push("C:\\Program Files (x86)\\LibreOffice\\program\\soffice.exe".into());
    } else if cfg!(target_os = "linux") {
        paths.push("/usr/lib/libreoffice/program/soffice".into());
        paths.push("/usr/bin/soffice".into());
        paths.push("/snap/bin/soffice".into());
    }
    paths
}

fn determine_lo_binary() -> String {
    if let Ok(lo_bin) = std::env::var("LO_BINARY") {
        if !lo_bin.is_empty() {
            return lo_bin;
        }
    }
    let config = load_config();
    // A bare "soffice" (the default) is not resolvable on macOS GUI apps, so
    // probe the known install paths first and only fall back to it.
    if !config.lo_binary_path.is_empty() && config.lo_binary_path != "soffice" {
        return config.lo_binary_path;
    }
    for path in common_lo_paths() {
        if Path::new(&path).exists() {
            return path;
        }
    }
    "soffice".to_string()
}

// ── Cached, verified LO binary — avoids per‑file `soffice --version`
//     spawns (each of which briefly registers a Dock icon). ────────────
static LO_BINARY: OnceLock<Option<String>> = OnceLock::new();

/// Cached, process‑lifetime resolution + liveness check.  Returns `None`
/// when LibreOffice is unavailable, else the absolute path to use.
pub fn lo_binary() -> Option<String> {
    LO_BINARY
        .get_or_init(|| {
            let bin = determine_lo_binary();
            if !bin.is_empty() && check_binary(&bin) {
                Some(bin)
            } else {
                None
            }
        })
        .clone()
}

/// Resolve the LibreOffice binary that will actually be used (respecting
/// LO_BINARY / config override, otherwise the first existing common path).
/// Used by the dependencies panel so users see the real path, not "soffice".
pub fn resolved_lo_binary() -> String {
    determine_lo_binary()
}

/// Suppress LibreOffice Dock icons during a scan session.
/// Sets LSUIElement=true in LO's Info.plist, restores on drop.
#[cfg(target_os = "macos")]
pub struct LoBackgroundGuard {
    plist: std::path::PathBuf,
}

#[cfg(target_os = "macos")]
impl LoBackgroundGuard {
    pub fn enter() -> Option<Self> {
        let binary = determine_lo_binary();
        if binary.is_empty() {
            return None;
        }

        // Resolve binary to .app bundle
        let binary_path = std::path::PathBuf::from(&binary);
        let app_dir = match find_lo_app_dir(&binary_path) {
            Some(d) => d,
            None => return None,
        };

        let plist = app_dir.join("Contents").join("Info.plist");
        if !plist.exists() {
            return None;
        }

        // Check if already set (e.g., left over from a crash)
        let already_set = std::process::Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :LSUIElement", plist.to_str()?])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("true"))
            .unwrap_or(false);

        if !already_set {
            let ok = std::process::Command::new("/usr/libexec/PlistBuddy")
                .args(["-c", "Add :LSUIElement bool true", plist.to_str()?])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                log::info!("[LO] Dock icon suppressed for scan session");
            }
        }

        Some(LoBackgroundGuard { plist })
    }

    /// On startup, clean up any LSUIElement left over from a crash
    pub fn recover() {
        let binary = determine_lo_binary();
        if binary.is_empty() {
            return;
        }
        let binary_path = std::path::PathBuf::from(&binary);
        let app_dir = match find_lo_app_dir(&binary_path) {
            Some(d) => d,
            None => return,
        };
        let plist = app_dir.join("Contents").join("Info.plist");
        if !plist.exists() {
            return;
        }
        let is_set = std::process::Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :LSUIElement", plist.to_str().unwrap_or("")])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("true"))
            .unwrap_or(false);
        if is_set {
            let ok = std::process::Command::new("/usr/libexec/PlistBuddy")
                .args(["-c", "Delete :LSUIElement", plist.to_str().unwrap_or("")])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                log::warn!("[LO] Restored Dock icon after crash recovery");
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for LoBackgroundGuard {
    fn drop(&mut self) {
        let ok = std::process::Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Delete :LSUIElement", self.plist.to_str().unwrap_or("")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            log::info!("[LO] Dock icon restored");
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub struct LoBackgroundGuard;

#[cfg(not(target_os = "macos"))]
impl LoBackgroundGuard {
    pub fn enter() -> Option<Self> {
        Some(LoBackgroundGuard)
    }
    pub fn recover() {}
}

/// Resolve a LibreOffice binary path to its .app bundle directory.
#[cfg(target_os = "macos")]
fn find_lo_app_dir(binary_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let canon = std::fs::canonicalize(binary_path).ok()?;
    let mut dir = canon;
    // Walk up to find "Contents" directory
    while dir.file_name().map(|n| n != "Contents").unwrap_or(false) {
        if !dir.pop() {
            return None;
        }
    }
    if dir.file_name().map(|n| n != "Contents").unwrap_or(true) {
        return None;
    }
    dir.pop(); // pop "Contents" to get .app
    Some(dir)
}

/// On startup, clean up any LSUIElement left over from a crash. Does NOT set
/// LSUIElement — Dock icons are suppressed per-scan via [`LoBackgroundGuard`].
#[cfg(target_os = "macos")]
pub fn ensure_lo_background_mode() {
    LoBackgroundGuard::recover();
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_lo_background_mode() {
    // No-op on non-macOS
}

// ── Request‑coalescing batcher — multiple incoming .doc / .ppt requests
//     from parallel Rayon threads are collected into a single soffice
//     invocation, reducing Dock‑icon flashes and serialising LO to
//     eliminate DeploymentException concurrency crashes. ───────────────

const LO_BATCH_SIZE: usize = 32;
const LO_BATCH_GRACE_MS: u64 = 300;

struct LoJob {
    path: PathBuf,
    tx: mpsc::Sender<Result<String, String>>,
}

struct LoBatchState {
    queue: VecDeque<LoJob>,
    collecting: bool,
}

pub struct LoBatcher {
    state: Mutex<LoBatchState>,
}

static LO_BATCHER: OnceLock<LoBatcher> = OnceLock::new();

fn lo_batcher() -> &'static LoBatcher {
    LO_BATCHER.get_or_init(|| LoBatcher {
        state: Mutex::new(LoBatchState {
            queue: VecDeque::new(),
            collecting: false,
        }),
    })
}

impl LoBatcher {
    /// Submit a path for LibreOffice conversion.  Calls arrive from
    /// parallel threads, but the batcher serialises and coalesces them
    /// so that a single `soffice` process handles many files.
    pub fn extract(&self, path: &Path) -> Result<String, String> {
        let (tx, rx) = mpsc::channel();
        let leader = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.queue.push_back(LoJob {
                path: path.to_path_buf(),
                tx,
            });
            if st.collecting {
                false
            } else {
                st.collecting = true;
                true
            }
        };
        if leader {
            // Brief grace so other threads have time to enqueue their
            // files before we drain the first batch.
            std::thread::sleep(Duration::from_millis(LO_BATCH_GRACE_MS));
            self.run_batches();
        }
        rx.recv()
            .unwrap_or_else(|_| Err("LibreOffice 批量调度异常".to_string()))
    }

    fn run_batches(&self) {
        loop {
            let batch: Vec<LoJob> = {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if st.queue.is_empty() {
                    st.collecting = false;
                    return;
                }
                let take = st.queue.len().min(LO_BATCH_SIZE);
                st.queue.drain(..take).collect()
            };
            let paths: Vec<PathBuf> = batch.iter().map(|j| j.path.clone()).collect();
            let results = extract_many_via_libreoffice(&paths);
            // extract_many always returns results.len() == paths.len().
            for (job, res) in batch.into_iter().zip(results.into_iter()) {
                let _ = job.tx.send(res);
            }
            // Loop — new jobs may have arrived while we were running
            // the batch (those callers are blocked on rx.recv()).
        }
    }
}

// ── Multi‑file & single‑file conversion ──────────────────────────────

/// Convert many files in one `soffice` process.  Handles filename‑stem
/// collisions internally by running extra sub‑rounds when necessary.
///
/// Returns one `Result` per input path, in the same order.
pub fn extract_many_via_libreoffice(paths: &[PathBuf]) -> Vec<Result<String, String>> {
    let n = paths.len();
    if n == 0 {
        return vec![];
    }
    let binary = match lo_binary() {
        Some(b) => b,
        None => {
            return paths
                .iter()
                .map(|_| Err("LibreOffice 未配置且不可用".to_string()))
                .collect();
        }
    };

    let tmp = match TempDir::new("ls_lo") {
        Ok(t) => t,
        Err(e) => return repeat_err(n, &format!("临时目录创建失败: {e}")),
    };
    let out_dir = tmp.path().to_path_buf();

    let profile_dir = out_dir.join("lo_profile");
    if let Err(e) = std::fs::create_dir_all(&profile_dir) {
        return repeat_err(n, &format!("profile 目录创建失败: {e}"));
    }
    let profile_display = profile_dir.display().to_string();
    let profile_uri = if profile_display.starts_with('/') {
        format!("file://{profile_display}")
    } else {
        format!("file:///{profile_display}")
    };

    // Group by lowercase stem so we never produce two output files with
    // the same name in one round (which would overwrite each other).
    let mut stem_map: HashMap<String, Vec<(usize, PathBuf)>> = HashMap::with_capacity(n);
    for (i, p) in paths.iter().enumerate() {
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_lowercase();
        stem_map.entry(stem).or_default().push((i, p.clone()));
    }
    let max_rounds = stem_map.values().map(|v| v.len()).max().unwrap_or(1);
    let mut results: Vec<Option<Result<String, String>>> = vec![None; n];

    for round in 0..max_rounds {
        let round_paths: Vec<(usize, PathBuf)> = stem_map
            .values()
            .filter_map(|entries| entries.get(round).cloned())
            .collect();
        if round_paths.is_empty() {
            break;
        }
        let round_n = round_paths.len();
        round_paths.iter().for_each(|(idx, _)| {
            results[*idx] = None;
        });
        let timeout = Duration::from_secs((30 + (15 * round_n) as u64).min(600));

        let stderr_log = out_dir.join("lo_stderr.log");
        let stderr_file = match std::fs::File::create(&stderr_log) {
            Ok(f) => f,
            Err(e) => {
                for (idx, _) in &round_paths {
                    results[*idx] =
                        Some(Err(format!("stderr 日志创建失败: {e}", )));
                }
                continue;
            }
        };

        let mut cmd = std::process::Command::new(&binary);
        cmd.env("SAL_USE_VCLPLUGIN", "svp")
            .arg(format!("-env:UserInstallation={profile_uri}"))
            .args([
                "--headless",
                "--nologo",
                "--nodefault",
                "--norestore",
                "--nolockcheck",
                "--nofirststartwizard",
            ])
            .arg("--convert-to")
            .arg("txt:Text")
            .arg("--outdir")
            .arg(&out_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(stderr_file));

        for (_, p) in &round_paths {
            cmd.arg(p);
        }

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                for (idx, _) in &round_paths {
                    results[*idx] = Some(Err(format!("{binary} 启动失败: {e}")));
                }
                continue;
            }
        };

        let child_handle = std::sync::Arc::new(std::sync::Mutex::new(child));
        let child_handle2 = std::sync::Arc::clone(&child_handle);
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            loop {
                let mut child = child_handle2.lock().unwrap_or_else(|e| e.into_inner());
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let _ = tx.send(Ok(status));
                        break;
                    }
                    Ok(None) => {
                        drop(child);
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });

        let _status = match rx.recv_timeout(timeout) {
            Ok(Ok(status)) => Some(status),
            Ok(Err(_e)) => None,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut child = child_handle.lock().unwrap_or_else(|e| e.into_inner());
                log::warn!("[OFFICE] killed soffice after {}s timeout", timeout.as_secs());
                let _ = child.kill();
                let _ = child.wait();
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        };

        // Collect per‑file results for this round.
        for (idx, path) in round_paths {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("output");
            let out_path = out_dir.join(format!("{stem}.txt"));
            let res = if out_path.exists() {
                match std::fs::read_to_string(&out_path) {
                    Ok(t) => Ok(t),
                    Err(e) => Err(format!("读取转换结果失败: {e}")),
                }
            } else {
                let stderr_snippet =
                    std::fs::read_to_string(&stderr_log).unwrap_or_default();
                Err(format!(
                    "转换失败: {}",
                    if stderr_snippet.trim().is_empty() {
                        "未生成输出文件"
                    } else {
                        stderr_snippet.trim()
                    }
                ))
            };
            results[idx] = Some(res);
            let _ = std::fs::remove_file(&out_path);
        }
    }

    // Drain the temp dir (in‑scope drop handles it, but explicit helps).
    drop(tmp);

    results
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err("内部错误".to_string())))
        .collect()
}

fn repeat_err(n: usize, msg: &str) -> Vec<Result<String, String>> {
    std::iter::repeat_with(|| Err(msg.to_string()))
        .take(n)
        .collect()
}

/// Convert a single file with LibreOffice (thin wrapper).
pub fn extract_via_libreoffice(path: &Path) -> anyhow::Result<String> {
    let mut results = extract_many_via_libreoffice(&[path.to_path_buf()]);
    results
        .pop()
        .unwrap_or_else(|| Err("内部错误".to_string()))
        .map_err(|e| anyhow::anyhow!("{e}"))
}

// ── Office extractor (native‑first, LO‑batched for legacy formats) ───

pub struct OfficeExtractor;

impl OfficeExtractor {
    pub fn new() -> Self {
        Self
    }

    fn lo_extract(path: &Path) -> anyhow::Result<String> {
        lo_batcher()
            .extract(path)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    fn native_then_lo(
        path: &Path,
        native: fn(&Path) -> anyhow::Result<String>,
    ) -> anyhow::Result<String> {
        match native(path) {
            Ok(t) if !t.trim().is_empty() => Ok(t),
            Ok(_) => {
                if lo_binary().is_some() {
                    Self::lo_extract(path)
                } else {
                    Ok(String::new())
                }
            }
            Err(native_err) => {
                if lo_binary().is_some() {
                    Self::lo_extract(path).map_err(|lo_err| {
                        anyhow::anyhow!("原生解析失败({native_err}); LibreOffice 也失败: {lo_err}")
                    })
                } else {
                    Err(native_err)
                }
            }
        }
    }

    fn lo_only(path: &Path) -> anyhow::Result<String> {
        if lo_binary().is_some() {
            Self::lo_extract(path)
        } else {
            Err(anyhow::anyhow!(
                "需要安装 LibreOffice 或使用自定义路径提取此格式。\
                 macOS: brew install --cask libreoffice\n\
                 Linux: sudo apt install libreoffice\n\
                 Windows: winget install LibreOffice。\
                 如需指定自定义路径，请在设置中配置 LibreOffice 可执行文件位置"
            ))
        }
    }
}

impl Extractor for OfficeExtractor {
    fn extract(&self, path: &Path) -> anyhow::Result<String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        match ext.as_str() {
            "docx" => Self::native_then_lo(path, extract_docx),
            "xlsx" | "xls" => Self::native_then_lo(path, extract_xlsx),
            "pptx" => Self::native_then_lo(path, extract_pptx),
            "doc" | "ppt" => Self::lo_only(path),
            _ => Err(anyhow::anyhow!("unsupported office format: {ext}")),
        }
    }
}

// ── Native parsers ───────────────────────────────────────────────────

fn extract_docx(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).context("failed to open DOCX")?;
    let mut archive = zip::ZipArchive::new(file).context("failed to read DOCX as ZIP")?;
    let entry = archive
        .by_name("word/document.xml")
        .context("DOCX missing word/document.xml")?;

    let mut xml_reader = XmlReader::from_reader(BufReader::new(entry));
    xml_reader.config_mut().trim_text(true);

    let mut text = String::new();
    let mut buf = Vec::new();
    let mut in_t = false;
    let mut in_p = false;

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                match e.local_name().as_ref() {
                    b"t" => in_t = true,
                    b"p" => in_p = true,
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                match e.local_name().as_ref() {
                    b"t" => in_t = false,
                    b"p" => {
                        if in_p {
                            text.push('\n');
                            in_p = false;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_t {
                    if let Ok(s) = e.unescape() {
                        text.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => anyhow::bail!("XML parse error in DOCX: {e}"),
            _ => {}
        }
        buf.clear();
    }

    if text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
}

fn extract_xlsx(path: &Path) -> Result<String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut text = String::new();

    if ext == "xls" {
        let mut workbook: calamine::Xls<_> =
            calamine::open_workbook(path).context("failed to open XLS")?;
        let sheets = workbook.sheet_names().to_vec();
        for name in &sheets {
            if let Ok(range) = workbook.worksheet_range(name) {
                for row in range.rows() {
                    let row_text: Vec<String> = row.iter().filter_map(|c| match c {
                        calamine::Data::String(s) => Some(s.clone()),
                        calamine::Data::Float(f) => Some(f.to_string()),
                        calamine::Data::Int(i) => Some(i.to_string()),
                        calamine::Data::Bool(b) => Some(b.to_string()),
                        calamine::Data::DateTime(dt) => Some(dt.to_string()),
                        calamine::Data::DateTimeIso(s) => Some(s.clone()),
                        calamine::Data::DurationIso(s) => Some(s.clone()),
                        _ => None,
                    }).collect();
                    if !row_text.is_empty() {
                        if !text.is_empty() { text.push('\n'); }
                        text.push_str(&row_text.join("\t"));
                    }
                }
            }
        }
    } else {
        let mut workbook: calamine::Xlsx<_> =
            calamine::open_workbook(path).context("failed to open XLSX")?;
        let sheets = workbook.sheet_names().to_vec();
        for name in &sheets {
            if let Ok(range) = workbook.worksheet_range(name) {
                for row in range.rows() {
                    let row_text: Vec<String> = row.iter().filter_map(|c| match c {
                        calamine::Data::String(s) => Some(s.clone()),
                        calamine::Data::Float(f) => Some(f.to_string()),
                        calamine::Data::Int(i) => Some(i.to_string()),
                        calamine::Data::Bool(b) => Some(b.to_string()),
                        calamine::Data::DateTime(dt) => Some(dt.to_string()),
                        calamine::Data::DateTimeIso(s) => Some(s.clone()),
                        calamine::Data::DurationIso(s) => Some(s.clone()),
                        _ => None,
                    }).collect();
                    if !row_text.is_empty() {
                        if !text.is_empty() { text.push('\n'); }
                        text.push_str(&row_text.join("\t"));
                    }
                }
            }
        }
    }

    Ok(text)
}

fn extract_pptx(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).context("failed to open PPTX")?;
    let mut archive = zip::ZipArchive::new(file).context("failed to read PPTX as ZIP")?;

    let mut slide_files: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let entry = archive.by_index(i).ok()?;
            let name = entry.name().to_string();
            if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    slide_files.sort_by(|a, b| {
        let a_num = extract_slide_number(a);
        let b_num = extract_slide_number(b);
        a_num.cmp(&b_num)
    });

    let mut text = String::new();

    for slide_name in &slide_files {
        let entry = archive
            .by_name(slide_name)
            .with_context(|| format!("failed to read {slide_name}"))?;

        let mut xml_reader = XmlReader::from_reader(BufReader::new(entry));
        xml_reader.config_mut().trim_text(true);

        let mut buf = Vec::new();
        let mut in_t = false;
        let mut slide_text = String::new();

        loop {
            match xml_reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    if e.local_name().as_ref() == b"t" {
                        in_t = true;
                    }
                }
                Ok(Event::End(ref e)) => {
                    if e.local_name().as_ref() == b"t" {
                        in_t = false;
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if in_t {
                        if let Ok(s) = e.unescape() {
                            slide_text.push_str(&s);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => anyhow::bail!("XML parse error in PPTX slide: {e}"),
                _ => {}
            }
            buf.clear();
        }

        if !slide_text.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&slide_text);
        }
    }

    Ok(text)
}

fn extract_slide_number(name: &str) -> usize {
    let stem = name.trim_end_matches(".xml");
    stem.rsplit_once("slide")
        .and_then(|(_, num)| num.parse::<usize>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;