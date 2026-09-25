//! Poppler CLI discovery (`pdftoppm` / `pdfimages` / `pdftotext` / `pdfinfo`)
//! and subprocess execution with timeout.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
/// Whether a poppler binary (`pdftoppm` / `pdfimages` / …) is installed: just
/// run it once and see if it resolves.
///
/// On Windows the process PATH is refreshed from the registry at startup and
/// after package installs (see [`crate::process::refresh_path`]), so a
/// winget/choco/scoop install is picked up with no directory enumeration. On
/// macOS/Linux a GUI app may not inherit the shell PATH, so the common install
/// prefixes remain as a fallback.
fn find_poppler_binary(name: &str) -> Option<PathBuf> {
    // poppler 的 pdftoppm/pdftotext 用 `-v`（非 `--version`），后者会报
    // "Couldn't open file '--version'"。统一用 `-v` 探测。
    const VERSION_FLAG: &str = "-v";
    if crate::process::probe_ok(name, &[VERSION_FLAG]) {
        return Some(PathBuf::from(name));
    }
    #[cfg(not(target_os = "windows"))]
    for prefix in crate::process::UNIX_BIN_PREFIXES {
        let candidate = PathBuf::from(prefix).join(name);
        if candidate.exists() && crate::process::probe_ok(&candidate, &[VERSION_FLAG]) {
            return Some(candidate);
        }
    }
    None
}

/// Caches only *positive* results: a "not found" is re-probed on the next call,
/// so installing poppler while the app is running flips the status to ready
/// without a restart.
fn cached(lock: &'static OnceLock<PathBuf>, name: &str) -> Option<&'static Path> {
    if let Some(p) = lock.get() {
        return Some(p.as_path());
    }
    let found = find_poppler_binary(name)?;
    Some(lock.get_or_init(|| found).as_path())
}

static PDFTOPPM_PATH: OnceLock<PathBuf> = OnceLock::new();
static PDFIMAGES_PATH: OnceLock<PathBuf> = OnceLock::new();
static PDFTOTEXT_PATH: OnceLock<PathBuf> = OnceLock::new();
static PDFINFO_PATH: OnceLock<PathBuf> = OnceLock::new();

pub(super) fn pdftoppm_path() -> Option<&'static Path> {
    cached(&PDFTOPPM_PATH, "pdftoppm")
}

pub(super) fn pdfimages_path() -> Option<&'static Path> {
    cached(&PDFIMAGES_PATH, "pdfimages")
}

pub(super) fn pdftotext_path() -> Option<&'static Path> {
    cached(&PDFTOTEXT_PATH, "pdftotext")
}

pub(super) fn pdfinfo_path() -> Option<&'static Path> {
    cached(&PDFINFO_PATH, "pdfinfo")
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

/// Longest page side in points from `pdfinfo` ("Page size: W x H pts"), so the
/// renderer can pick a DPI without exploding on oversized pages. `None` when
/// pdfinfo is unavailable or the page size is missing.
pub(super) fn pdf_longest_side_pt(path: &Path) -> Option<f64> {
    let bin = pdfinfo_path()?;
    let mut cmd = crate::process::new(bin);
    cmd.arg(path);
    let (Some(status), stdout) = run_with_timeout(cmd, Duration::from_secs(60)).ok()? else {
        return None;
    };
    if !status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&stdout);
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("Page size:") {
            let nums: Vec<f64> = v
                .split_whitespace()
                .filter_map(|t| t.parse::<f64>().ok())
                .collect();
            if nums.len() >= 2 {
                return Some(nums[0].max(nums[1]));
            }
        }
    }
    None
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
