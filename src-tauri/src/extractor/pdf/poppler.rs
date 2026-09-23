//! Poppler CLI discovery (`pdftoppm` / `pdfimages` / `pdftotext` / `pdfinfo`)
//! and subprocess execution with timeout.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
/// Locate a poppler binary (`pdftoppm` or `pdfimages`).
/// Searches PATH first, then the bundled/dev `poppler-bin/` dir, then next
/// to the executable, then platform install prefixes — the Tauri app may
/// not inherit the terminal PATH.
fn find_poppler_binary(name: &str) -> Option<PathBuf> {
    // poppler 的 pdftoppm/pdftotext 用 `-v`（非 `--version`），后者会报
    // "Couldn't open file '--version'"。统一用 `-v` 探测。
    const VERSION_FLAG: &str = "-v";
    // On Windows, probe with the .exe name first; the bare-name PATH probe
    // can still match via PATHEXT, but CreateProcess needs the real file.
    if crate::process::probe_ok(name, &[VERSION_FLAG]) {
        return Some(PathBuf::from(name));
    }
    // Dev mode: look relative to project root
    let dev_name = crate::process::windows_exe_name(name);
    let dev_path = PathBuf::from("poppler-bin").join(&dev_name);
    if dev_path.exists() && crate::process::probe_ok(&dev_path, &[VERSION_FLAG]) {
        return Some(dev_path);
    }
    // Release mode: look next to the executable
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent() {
            let bundle_path = dir.join(&dev_name);
            if bundle_path.exists() && crate::process::probe_ok(&bundle_path, &[VERSION_FLAG]) {
                return Some(bundle_path);
            }
        }
    #[cfg(target_os = "windows")]
    for prefix in ["C:\\Program Files\\poppler\\Library\\bin", "C:\\poppler\\Library\\bin", "C:\\Program Files\\poppler\\bin"] {
        let candidate = PathBuf::from(prefix).join(&dev_name);
        if candidate.exists() && crate::process::probe_ok(&candidate, &[VERSION_FLAG]) {
            return Some(candidate);
        }
    }
    #[cfg(not(target_os = "windows"))]
    for prefix in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let candidate = PathBuf::from(prefix).join(&dev_name);
        if candidate.exists() && crate::process::probe_ok(&candidate, &[VERSION_FLAG]) {
            return Some(candidate);
        }
    }
    None
}

static PDFTOPPM_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static PDFIMAGES_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

pub(super) fn pdftoppm_path() -> Option<&'static Path> {
    PDFTOPPM_PATH.get_or_init(|| find_poppler_binary("pdftoppm")).as_deref()
}

pub(super) fn pdfimages_path() -> Option<&'static Path> {
    PDFIMAGES_PATH.get_or_init(|| find_poppler_binary("pdfimages")).as_deref()
}

static PDFTOTEXT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

pub(super) fn pdftotext_path() -> Option<&'static Path> {
    PDFTOTEXT_PATH.get_or_init(|| find_poppler_binary("pdftotext")).as_deref()
}

static PDFINFO_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

pub(super) fn pdfinfo_path() -> Option<&'static Path> {
    PDFINFO_PATH.get_or_init(|| find_poppler_binary("pdfinfo")).as_deref()
}

/// Get the number of pages in a PDF. Uses pdfinfo first (tolerant of
/// malformed PDFs that lopdf rejects), falling back to lopdf.
pub(crate) fn get_pdf_page_count(path: &Path) -> Result<u32> {
    // Try pdfinfo first — handles broken streams that lopdf rejects
    if let Some(bin) = pdfinfo_path() {
        let mut cmd = crate::process::new(bin);
        cmd.arg(path);
        let (status, stdout) = run_with_timeout(cmd, Duration::from_secs(60))
            .unwrap_or((None, Vec::new()));
        if status.map(|s| s.success()).unwrap_or(false) {
            let stdout = String::from_utf8_lossy(&stdout);
            for line in stdout.lines() {
                if let Some(val) = line.strip_prefix("Pages:") {
                    return val.trim().parse::<u32>().context("invalid pdfinfo Pages output");
                }
            }
        }
    }
    // Fall back to lopdf
    let doc = lopdf::Document::load(path).context("failed to load PDF")?;
    Ok(doc.get_pages().len() as u32)
}

/// Run a command with a timeout, capturing stdout. A broken/crafted PDF can
/// hang poppler forever; the timeout lets the scan worker move on. A timeout
/// yields `Ok((None, vec![]))` so callers can fall back.
pub(super) fn run_with_timeout(mut cmd: std::process::Command, timeout: Duration) -> std::io::Result<(Option<std::process::ExitStatus>, Vec<u8>)> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(st) => break st,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok((None, Vec::new()));
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    let mut out = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        std::io::Read::read_to_end(&mut so, &mut out)?;
    }
    Ok((Some(status), out))
}
/// Check if pdftoppm is available on the system.
pub fn is_pdftoppm_available() -> bool {
    pdftoppm_path().is_some()
}

/// Check if pdfimages is available on the system.
pub fn is_pdfimages_available() -> bool {
    pdfimages_path().is_some()
}

/// Whether the poppler binaries the scanned-PDF OCR fallback needs
/// (`pdftoppm` AND `pdfimages`) are both runnable; either missing silently
/// disables image-layer OCR.
pub fn poppler_available() -> bool {
    is_pdftoppm_available() && is_pdfimages_available()
}
