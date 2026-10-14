//! Scanned-PDF OCR pipeline: whole-doc fast path (pdfimages / pdftoppm) and
//! per-page OCR guarded by a progress-based stall watchdog.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::scanner::helpers::TempDir;

use super::poppler::{pdf_longest_side_pt, pdfimages_path, pdftoppm_path};
use super::quality::{is_garbled_text, is_repetitive, is_sparse_text_layer};
use super::{
    get_pdf_page_count, global_ocr_stall_timeout, global_pdf_dpi, should_ocr_pages_individually,
    LARGE_SCAN_PAGE_THRESHOLD,
};

/// Upper bound on the longest side (px) of a rendered page. Oversized pages
/// (e.g. a 3000×4000pt scan) would otherwise rasterize to >200 Mpx — slow and
/// above the OCR engine's image-decode limit.
///
/// Aliases [`crate::extractor::preprocess::MAX_LONGEST_SIDE`] so a rendered page
/// is never larger than what the OCR pipeline would keep: rendering 2500px only
/// to have the preprocessor shrink it again would waste the render (subprocess +
/// PNG decode) for nothing.
const MAX_RENDER_LONGEST_SIDE: u32 = crate::extractor::preprocess::MAX_LONGEST_SIDE;
const MIN_RENDER_LONGEST_SIDE: u32 = 1000;

/// The largest image on a page smaller than this is an icon/logo, not a scan.
const MIN_PAGE_IMAGE_AREA: u64 = 100_000;

/// Target longest-side pixels for rendering at `dpi`, clamped so a giant page
/// can't explode. Falls back to the cap when the page size is unknown.
fn render_longest_side(path: &Path, dpi: u32) -> u32 {
    let target = pdf_longest_side_pt(path)
        .map(|pt| (dpi as f64 / 72.0 * pt).round() as u32)
        .unwrap_or(MAX_RENDER_LONGEST_SIDE);
    target.clamp(MIN_RENDER_LONGEST_SIDE, MAX_RENDER_LONGEST_SIDE)
}
/// How many pages of **one** document may be OCR'd concurrently.
///
/// Each rendered page spawns its own `pdftoppm` subprocess, so this must stay
/// small (a 600-page document must not launch 600 processes at once) — but 1 is
/// far too small. Measured 2026-10-13: `保利股份2014年年报.pdf` (188 scanned
/// pages, render path) spent ~29 s per page on a single thread ≈ **91 minutes**,
/// while the global OCR gate (8 slots on this machine, 6 physical cores) sat
/// idle and the session averaged 12% CPU. Four keeps one document inside half
/// the gate, so parallel documents still make progress.
pub(super) const PAGE_OCR_WINDOW: usize = 4;

/// Outcome of a windowed page-OCR pass (see [`run_pages_windowed`]).
pub(super) struct PageOcrOutcome {
    /// One slot per input page, in the same order: `None` = no text produced.
    pub(super) texts: Vec<Option<String>>,
    /// How many pages were actually handed to `ocr_page`.
    pub(super) attempted: usize,
    /// The stall watchdog tripped, or `give_up_after` fired: dispatch stopped.
    pub(super) stalled: bool,
    /// The scan was cancelled mid-pass.
    pub(super) cancelled: bool,
    /// Memory pressure hit `Paused`: dispatching stopped before every page ran.
    pub(super) paused: bool,
    /// Highest number of pages that were in flight at the same time.
    pub(super) peak_concurrency: usize,
}

/// OCR `pages` with at most [`PAGE_OCR_WINDOW`] pages in flight.
///
/// Pure scheduling — `ocr_page` does the work and its own logging. Semantics
/// mirror the serial loops this replaces:
/// - cancellation is checked before each **new** page; pages already in flight
///   finish, so their text is never thrown away;
/// - `stall` is a **progress** watchdog, not a per-page timeout: *any* page that
///   finishes counts as progress and the budget is scaled by the window (pages run
///   concurrently, so one page legitimately takes ~`window`× longer — measured
///   171 s dense pages under a 4-wide window). Only a pass that produces no
///   finished page for `stall × window` stops dispatching (in-flight pages still
///   finish and keep their results); a page that merely exceeds `stall` is logged
///   as slow and the pass continues;
/// - `give_up_after` stops the pass once that many pages have been attempted and
///   **none** produced text (the original "first 5 blank pages" guard, which is
///   what keeps a text-less scan from rendering all of its pages).
pub(super) fn run_pages_windowed(
    pages: &[u32],
    stall: Option<Duration>,
    give_up_after: Option<usize>,
    // Injected so tests are deterministic: the real callers pass
    // `crate::extractor::scan_cancelled`, but that is a process-global flag which
    // other (parallel) tests can set, making tests flaky.
    is_cancelled: impl Fn() -> bool + Sync,
    ocr_page: impl Fn(u32) -> Option<String> + Sync,
) -> PageOcrOutcome {
    let slots: Mutex<Vec<Option<String>>> = Mutex::new(vec![None; pages.len()]);
    let next = AtomicUsize::new(0);
    let attempted = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let stalled = AtomicBool::new(false);
    let cancelled = AtomicBool::new(false);
    let paused = AtomicBool::new(false);
    let window = crate::load::page_window().max(1).min(pages.len().max(1));
    // 进度以「自本轮开始以来的单调时刻」记录 —— 不能用每页各自的耗时（各 worker 之间
    // 不可比，相减会下溢：debug 直接 panic，实测就是它让单测偶发失败的）。
    let pass_started = Instant::now();
    let last_progress = AtomicU64::new(0);
    // 观测：实际同时进行的页数（窗口是否真的被用满）。只用于日志归因。
    let live = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..window {
            scope.spawn(|| loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if is_cancelled() {
                    cancelled.store(true, Ordering::Relaxed);
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                // 内存压力到暂停档：不再派发新页（在飞页照常跑完）。调用方据此把该文档
                // 判为失败并稍后重试，绝不把「只 OCR 了一半」的内容当成成功写进索引。
                if crate::load::is_paused() {
                    paused.store(true, Ordering::Relaxed);
                    stop.store(true, Ordering::Relaxed);
                    crate::load::note_paused_in_pass();
                    log::warn!(
                        "[LOAD] 内存压力过高（暂停档）：停止派发剩余页（已尝试 {}/{} 页），本次提取判失败稍后重试",
                        attempted.load(Ordering::Relaxed),
                        pages.len()
                    );
                    break;
                }
                let slot = next.fetch_add(1, Ordering::Relaxed);
                let Some(&page) = pages.get(slot) else { break };
                attempted.fetch_add(1, Ordering::Relaxed);

                let started = Instant::now();
                let now_live = live.fetch_add(1, Ordering::Relaxed) + 1;
                peak.fetch_max(now_live, Ordering::Relaxed);
                let text = ocr_page(page);
                live.fetch_sub(1, Ordering::Relaxed);
                if text.is_some() {
                    let mut guard = slots.lock().unwrap_or_else(|e| e.into_inner());
                    guard[slot] = text;
                }

                let elapsed = started.elapsed();
                // 进度看门狗：**任何一页完成**都算进展。用「自本轮开始以来的单调时刻」
                // 而不是「本页耗时」——各 worker 的耗时互相不可比，相减会下溢（debug
                // 直接 panic，这正是实测到的单测偶发失败原因）。
                let now_ms = pass_started.elapsed().as_millis() as u64;
                let prev_ms = last_progress.swap(now_ms, Ordering::Relaxed);
                let gap_ms = now_ms.saturating_sub(prev_ms);
                if let Some(stall) = stall {
                    if elapsed >= stall {
                        log::warn!(
                            "[PDF] page {page} took {elapsed:?} (>= stall {stall:?}) — slow, but the pass is still progressing"
                        );
                    }
                    // 预算按窗口放大：并发下单页必然更慢（实测 4 并发下密集页 171s）。
                    // 只有「连续预算时长内一页都没完成」才算卡死——按单页耗时判定会在
                    // 188 页扫描件上于第 14 页就误判中止（2026-10-13 实测）。
                    let budget = stall * window as u32;
                    if gap_ms >= budget.as_millis() as u64 {
                        log::warn!(
                            "[PDF] 连续 {gap_ms} ms 没有任何一页完成（>= {budget:?}）——判定卡死，停止派发"
                        );
                        stalled.store(true, Ordering::Relaxed);
                        stop.store(true, Ordering::Relaxed);
                        continue;
                    }
                }
                if let Some(limit) = give_up_after {
                    let anywhere = slots
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                        .any(|t| t.is_some());
                    if !anywhere && attempted.load(Ordering::Relaxed) >= limit {
                        stalled.store(true, Ordering::Relaxed);
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    PageOcrOutcome {
        texts: slots.into_inner().unwrap_or_else(|e| e.into_inner()),
        attempted: attempted.load(Ordering::Relaxed),
        stalled: stalled.load(Ordering::Relaxed),
        cancelled: cancelled.load(Ordering::Relaxed),
        paused: paused.load(Ordering::Relaxed),
        peak_concurrency: peak.load(Ordering::Relaxed),
    }
}

pub(super) fn run_pdf_ocr_pipeline(
    path: &Path,
    page_count: usize,
    page_texts: &[String],
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
    skip_pdfimages: bool,
) -> Option<String> {
    let page_count = if page_count == 0 {
        get_pdf_page_count(path).unwrap_or(0) as usize
    } else {
        page_count
    };

    if !should_ocr_pages_individually(page_count, false) {
        if let Some(ocr_text) = try_ocr_fallback(path, lang, engine, true, skip_pdfimages) {
            return Some(ocr_text);
        }
    } else {
        log::info!(
            "[PDF] {:?}: large scanned PDF ({} pages > {}), attempting fast-path whole-doc OCR first",
            path.file_name(), page_count, LARGE_SCAN_PAGE_THRESHOLD
        );
        // Skip whole-doc pdftoppm for large docs: rendering every page at once
        // takes minutes and reliably trips the 120s timeout, after which the
        // per-page loop below redoes the same work in parallel anyway.
        if let Some(ocr_text) = try_ocr_fallback(path, lang, engine, false, skip_pdfimages) {
            return Some(ocr_text);
        }
    }

    if should_ocr_pages_individually(page_count, true) {
        log::info!(
            "[PDF] {:?}: running per-page OCR loop for {} pages (stall watchdog {:?})",
            path.file_name(), page_count, global_ocr_stall_timeout()
        );
        let dpi = global_pdf_dpi();
        let stall = global_ocr_stall_timeout();
        let all_pages: Vec<u32> = (1..=page_count as u32).collect();
        let pass_t0 = Instant::now();
        let outcome = run_pages_windowed(&all_pages, stall, Some(5), crate::extractor::scan_cancelled, |page| {
            let started = Instant::now();
            let text = ocr_single_pdf_page(path, page, dpi, lang, engine);
            // 这一路径原本整段无日志（188 页的扫描件十几分钟一行不写，看起来像卡死），
            // 现在按页 INFO，与 pdfimages 路径保持同样的可观测性。
            log::info!(
                "[PDF] {:?} page {page}: {} chars in {:.1}s (render)",
                path.file_name(),
                text.as_deref().map(|t| t.chars().count()).unwrap_or(0),
                started.elapsed().as_secs_f64()
            );
            text
        });
        if outcome.cancelled {
            log::info!(
                "[PDF] {:?}: 扫描已取消, 停止逐页 OCR（已完成 {}/{} 页）",
                path.file_name(),
                outcome.texts.iter().filter(|t| t.is_some()).count(),
                page_count
            );
        }
        // 0-based 页号 → 文本，与 `merge_page_texts` 的约定一致。
        let ocr_pages: HashMap<usize, String> = outcome
            .texts
            .into_iter()
            .enumerate()
            .filter_map(|(idx, text)| text.map(|t| (idx, t)))
            .collect();

        let pass_secs = pass_t0.elapsed().as_secs_f64().max(0.001);
        log::info!(
            "[PDF] {:?}: per-page OCR completed: attempted {}/{} pages, got text for {} pages, {:.1}s ({:.1} pages/min), peak concurrency {}, stalled={}, paused={}",
            path.file_name(), outcome.attempted, page_count, ocr_pages.len(),
            pass_secs, ocr_pages.len() as f64 * 60.0 / pass_secs, outcome.peak_concurrency, outcome.stalled, outcome.paused
        );

        if !ocr_pages.is_empty() {
            let merged = merge_page_texts(page_texts, &ocr_pages);
            if !merged.trim().is_empty() {
                return Some(merged);
            }
        }
    }

    None
}

/// Try OCR via pdfimages → (optionally) pdftoppm, returning the first usable
/// result. `allow_whole_doc_pdftoppm` is false for large documents, where the
/// all-pages render is abandoned in favour of the budgeted per-page loop.
/// `skip_pdfimages` is set for pages carrying `/Rotate` 90°/270°: `pdfimages`
/// copies the raw embedded image without applying the rotation, so it only
/// yields garbage — go straight to `pdftoppm` (which bakes it in).
fn try_ocr_fallback(
    path: &Path,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
    allow_whole_doc_pdftoppm: bool,
    skip_pdfimages: bool,
) -> Option<String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    // pdfimages 结果被判乱码时先留着：万一 pdftoppm 也失败，宁可回退到它，
    // 也不要因为一个误判把整篇内容丢掉。
    let mut rejected: Option<String> = None;
    if pdfimages_path().is_some() && !skip_pdfimages {
        match ocr_pdf_via_pdfimages(path, lang, engine) {
            Ok(ocr_text) if ocr_text.len() > 100 => {
                // `len > 100` alone let rotated-page garbage through (~all real
                // hits were one char per line). Reject it and retry with the
                // rotation-aware renderer below.
                if ocr_text_is_unusable(&ocr_text) {
                    log::warn!(
                        "[PDF] pdfimages OCR for {name:?} looks garbled ({} chars) — retrying via pdftoppm (applies page /Rotate)",
                        ocr_text.len(),
                    );
                    rejected = Some(ocr_text);
                } else {
                    log::info!("[PDF] pdfimages OCR for {name:?} (OCR'd {} chars)", ocr_text.len());
                    return Some(ocr_text);
                }
            }
            Ok(_) => log::info!("[PDF] pdfimages OCR returned empty text for {name:?}"),
            // Expected routing outcomes (not a scanned PDF / no single-JPEG
            // page) land here and fall through to the renderer — INFO, not WARN.
            Err(e) => log::info!("[PDF] pdfimages path skipped for {name:?}: {e}"),
        }
    } else if skip_pdfimages {
        log::info!("[PDF] {name:?}: pages carry /Rotate — skipping pdfimages (it ignores page rotation)");
    }
    if allow_whole_doc_pdftoppm && pdftoppm_path().is_some() {
        match ocr_pdf_via_pdftoppm(path, lang, engine) {
            Ok(ocr_text) if !ocr_text.is_empty() => {
                log::info!("[PDF] pdftoppm OCR for {name:?} (OCR'd {} chars)", ocr_text.len());
                return Some(ocr_text);
            }
            Ok(_) => log::warn!("[PDF] pdftoppm OCR returned empty text for {name:?}"),
            Err(e) => log::warn!("[PDF] pdftoppm OCR failed for {name:?}: {e}"),
        }
    }
    if let Some(text) = rejected {
        log::warn!("[PDF] {name:?}: pdftoppm yielded nothing — keeping the (suspect) pdfimages text");
        return Some(text);
    }
    None
}

/// Whether pdfimages-OCR output is unusable and must be retried with pdftoppm.
///
/// The old gate was `len > 100`, which rotated-page garbage trivially passes:
/// OCR of a sideways page emits roughly one character per line (`"0\n冈\n半\n…"`),
/// so the text looks long but carries no words. Reuse the same quality metrics
/// the indexer uses, plus an explicit "newlines outnumber text" check.
fn ocr_text_is_unusable(text: &str) -> bool {
    use crate::extractor::quality::{ExtractMeta, QualityFlag, compute_quality};
    if is_garbled_text(text) {
        return true;
    }
    let newlines = text.chars().filter(|c| *c == '\n').count();
    let non_ws = text.chars().filter(|c| !c.is_whitespace()).count();
    // 换行比正文还密 ⇒ 每个字被拆成一行（旋转/误读的典型形态）
    if non_ws > 0 && newlines * 2 >= non_ws {
        return true;
    }
    let q = compute_quality(
        text,
        &ExtractMeta { ocr_used: true, ..Default::default() },
        "pdf",
    );
    q.flags.iter().any(|f| {
        matches!(
            f,
            QualityFlag::LowPrintable | QualityFlag::LowLexicon | QualityFlag::HighFffd
        )
    })
}
pub(super) fn page_needs_ocr(page_text: &str, page_has_images: bool) -> bool {
    if is_sparse_text_layer(page_text, 1) {
        return true;
    }
    if is_garbled_text(page_text) {
        return true;
    }
    if page_has_images && is_repetitive(page_text) {
        return true;
    }
    false
}

/// Splice per-page OCR results into the text layer, one line per page.
///
/// The page count is `max(text_layer.len(), highest OCR page + 1)` — the caller
/// may pass an **empty** text layer (the "no text layer at all" path forces OCR
/// with `&[]`), in which case iterating `text_layer` alone would drop every
/// OCR'd page and return an empty string. That regression silently discarded
/// 142 of 145 pages on a real scanned volume.
pub(super) fn merge_page_texts(text_layer: &[String], ocr_pages: &HashMap<usize, String>) -> String {
    let ocr_len = ocr_pages.keys().max().map(|m| m + 1).unwrap_or(0);
    let page_count = text_layer.len().max(ocr_len);
    let mut parts: Vec<&str> = Vec::with_capacity(page_count);
    for i in 0..page_count {
        if let Some(ocr) = ocr_pages.get(&i) {
            parts.push(ocr.as_str());
        } else if let Some(tl) = text_layer.get(i) {
            parts.push(tl.as_str());
        }
    }
    parts.join("\n")
}

pub(super) fn ocr_single_pdf_page(
    path: &Path,
    page_number: u32,
    dpi: u32,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
) -> Option<String> {
    let bin = pdftoppm_path()?;
    let tmp_dir = TempDir::new("ls_pdf_page_ocr").ok()?;
    let output_prefix = tmp_dir.path().join("page");

    let scale = render_longest_side(path, dpi).to_string();
    let mut cmd = crate::process::new(bin);
    cmd.args([
        "-png",
        "-scale-to",
        &scale,
        "-f",
        &page_number.to_string(),
        "-l",
        &page_number.to_string(),
    ])
    .arg(path)
    .arg(&output_prefix);
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }

    let page_file = std::fs::read_dir(tmp_dir.path())
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("png"))?;

    let text = crate::extractor::ocr::ocr_image_with_engine(&page_file, engine, lang).ok()?;
    let trimmed = text.trim().to_owned();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

/// Render PDF pages to images using pdftoppm and run OCR.
/// Returns extracted text from all pages.
pub fn ocr_pdf_via_pdftoppm(
    path: &Path,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
) -> Result<String> {
    let tmp_dir = TempDir::new("ls_pdf_ocr")?;
    log::info!("[PDF] pdftoppm: rendering {:?}", path.file_name());

    let output_prefix = tmp_dir.path().join("page");
    let bin = pdftoppm_path()
        .ok_or_else(|| anyhow::anyhow!("pdftoppm not available. Install poppler-utils."))?;
    let mut cmd = crate::process::new(bin);
    let dpi = global_pdf_dpi();
    let scale = render_longest_side(path, dpi).to_string();
    cmd.args(["-png", "-scale-to", &scale]).arg(path).arg(&output_prefix);
    cmd.stderr(Stdio::null());
    let mut child = cmd.spawn()
        .map_err(|e| anyhow::anyhow!("pdftoppm not available: {e}. Install poppler-utils."))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow::anyhow!("pdftoppm timed out after 120s"));
            }
            Err(e) => return Err(anyhow::anyhow!("pdftoppm error: {e}")),
        }
    };
    if !status.success() {
        return Err(anyhow::anyhow!("pdftoppm failed to render PDF"));
    }

    let page_files: Vec<_> = (1..).map(|n| tmp_dir.path().join(format!("page-{n}.png")))
        .take_while(|p| p.exists()).collect();
    log::info!(
        "[PDF] {:?}: {} page images, starting OCR ({}) [engine={:?}]",
        path.file_name(),
        page_files.len(),
        lang,
        engine,
    );

    use rayon::prelude::*;
    let page_texts: Vec<Option<String>> = page_files
        .par_iter()
        .map(|page_path| {
            let page_no = page_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_owned();
            let started = std::time::Instant::now();
            let result = crate::extractor::ocr::ocr_image_with_regions(page_path, engine, lang);
            match result {
                Ok((text, regions)) => {
                    log::info!(
                        "[PDF] page {}: {} chars from {} regions in {:.1}s",
                        page_no,
                        text.len(),
                        regions,
                        started.elapsed().as_secs_f64(),
                    );
                    Some(text)
                        .map(|t| t.trim().to_owned())
                        .filter(|t| !t.is_empty())
                }
                Err(e) => {
                    log::warn!("[PDF] page {} OCR failed: {e}", page_no);
                    None
                }
            }
        })
        .collect();

    let mut full_text = String::new();
    for text in page_texts.into_iter().flatten() {
        if !full_text.is_empty() {
            full_text.push('\n');
        }
        full_text.push_str(&text);
    }

    Ok(full_text)
}


/// Render scanned PDF pages via pdfimages (extracts only the image layer,
/// not overlays/annotations/watermarks). Returns the OCR'd text with far
/// less watermark contamination than pdftoppm-based rendering.
///
/// A single whole-document `pdfimages` pass extracts every page image at once
/// (`-p` encodes the page number in each filename). The previous per-page
/// approach re-parsed the whole PDF once per page — on 40-page/80MB scans that
/// meant 40 process spawns racing a 30s timeout each (measured: whole-doc pass
/// ~0.1s, per-page frequently timed out under load).
pub fn ocr_pdf_via_pdfimages(
    path: &Path,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
) -> Result<String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_owned();
    log::info!("[PDF] pdfimages: extracting {name:?}");

    let page_count = get_pdf_page_count(path)? as usize;
    if page_count == 0 {
        return Ok(String::new());
    }

    // Optional cheap pre-check from `pdfimages -list`. It can time out on huge
    // PDFs, so it is an optimization only — the per-page strategy is decided
    // from the actually-extracted files below.
    if let Some(info) = super::scan::image_list_info(path) {
        // A digital/vector PDF with few (or zero) page images is NOT a scan.
        if info.pages_with_image == 0
            || (info.total_pages > 2 && info.pages_with_image * 2 < info.total_pages)
        {
            return Err(anyhow::anyhow!(
                "pdfimages: only {}/{} pages have images — not a scanned PDF",
                info.pages_with_image, info.total_pages
            ));
        }
        if (1..=page_count as u32).all(|p| !info.is_single_jpeg(p)) {
            return Err(anyhow::anyhow!(
                "pdfimages: no single-JPEG page among {page_count} — using renderer"
            ));
        }
    }

    let tmp = TempDir::new("ls_pdfimg")?;
    let prefix = tmp.path().join("img");
    let stall = global_ocr_stall_timeout();
    run_pdfimages_doc(path, "-j", &prefix, stall)?;
    let per_page = collect_page_images(tmp.path(), page_count)?;

    // Must still look like a scan: most pages carry an image.
    let pages_with_image = per_page.iter().filter(|v| !v.is_empty()).count();
    if page_count > 2 && pages_with_image * 2 < page_count {
        return Err(anyhow::anyhow!(
            "pdfimages: only {pages_with_image}/{page_count} pages had images — not a scanned PDF"
        ));
    }

    // Per-page strategy from the extracted files. Only a page with exactly one
    // JPEG is safe for the fast path; pages with multiple images (a near-blank
    // background plus CCITT/JBIG2 stencil masks holding the real text) or a
    // non-JPEG file (Netpbm produced by `-j`) are rendered instead — otherwise
    // the OCR engine gets a file it cannot decode (`0x88982F50`).
    let fast: Vec<bool> = per_page.iter().map(|imgs| is_single_jpeg_file(imgs)).collect();
    let fast_count = fast.iter().filter(|&&b| b).count();
    if fast_count == 0 {
        return Err(anyhow::anyhow!(
            "pdfimages: no single-JPEG page among {page_count} — using renderer"
        ));
    }
    log::info!(
        "[PDF] {name:?}: {page_count} pages — {fast_count} single-JPEG (fast), {} rendered",
        page_count - fast_count
    );

    let dpi = global_pdf_dpi();
    let mut out: Vec<Option<String>> = vec![None; page_count];

    // Fast pages: OCR the single JPEG in parallel (cheap).
    use rayon::prelude::*;
    let fast_idx: Vec<usize> = (0..page_count).filter(|&i| fast[i]).collect();
    let fast_text: Vec<Option<String>> = fast_idx
        .par_iter()
        .map(|&idx| {
            if crate::extractor::scan_cancelled() {
                return None;
            }
            let (img_path, area) = per_page.get(idx)?.first()?;
            if *area < MIN_PAGE_IMAGE_AREA {
                return None;
            }
            let started = std::time::Instant::now();
            match crate::extractor::ocr::ocr_image_with_regions(img_path, engine, lang) {
                Ok((t, _)) => {
                    log::info!(
                        "[PDF] {name:?} page {}: {} chars in {:.1}s (pdfimages)",
                        idx + 1,
                        t.chars().count(),
                        started.elapsed().as_secs_f64()
                    );
                    Some(t)
                }
                Err(e) => {
                    log::warn!("[PDF] {name:?} page {}: pdfimages OCR failed: {e}", idx + 1);
                    None
                }
            }
        })
        .collect();
    for (i, idx) in fast_idx.iter().enumerate() {
        out[*idx] = fast_text[i]
            .as_ref()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());
    }

    // Rendered pages: `pdftoppm` composites background + stencils correctly.
    // Bounded window rather than strictly serial — each page spawns its own
    // `pdftoppm` subprocess (see [`PAGE_OCR_WINDOW`]); running these one at a
    // time is what made a 188-page scan take ~91 minutes on a single thread.
    let render_pages: Vec<u32> = (0..page_count)
        .filter(|&i| !fast[i])
        .map(|i| (i + 1) as u32)
        .collect();
    let pass_t0 = std::time::Instant::now();
    let outcome = run_pages_windowed(&render_pages, stall, None, crate::extractor::scan_cancelled, |page| {
        let started = std::time::Instant::now();
        let text = ocr_single_pdf_page(path, page, dpi, lang, engine);
        match text.as_deref() {
            Some(t) => log::info!(
                "[PDF] {name:?} page {page}: {} chars in {:.1}s (render)",
                t.chars().count(),
                started.elapsed().as_secs_f64()
            ),
            None => log::warn!("[PDF] {name:?} page {page}: render produced no text"),
        }
        text
    });
    if outcome.cancelled {
        log::info!(
            "[PDF] {name:?}: 扫描已取消, 停止渲染页 OCR（已完成 {}/{} 页）",
            outcome.texts.iter().filter(|t| t.is_some()).count(),
            render_pages.len()
        );
    }
    {
        // 每文档计时汇总：以后任何一轮都能直接从日志算出「这篇花了多久、拿到了几页、
        // 窗口有没有真的用满」，不用事后靠外部采样拼凑。
        let got = outcome.texts.iter().filter(|t| t.is_some()).count();
        let secs = pass_t0.elapsed().as_secs_f64().max(0.001);
        log::info!(
            "[PDF] {name:?}: rendered-page OCR completed: {}/{} pages with text, {:.1}s ({:.1} pages/min), peak concurrency {}, stalled={}, paused={}",
            got,
            render_pages.len(),
            secs,
            got as f64 * 60.0 / secs,
            outcome.peak_concurrency,
            outcome.stalled,
            outcome.paused
        );
    }
    for (slot, text) in outcome.texts.into_iter().enumerate() {
        if let Some(t) = text {
            out[render_pages[slot] as usize - 1] = Some(t);
        }
    }

    let mut full_text = String::new();
    for text in out.into_iter().flatten() {
        if !full_text.is_empty() {
            full_text.push('\n');
        }
        full_text.push_str(&text);
    }

    Ok(full_text)
}

/// Snapshot of `pdfimages` output used by the stall watchdog: (file count, total
/// bytes) for entries whose stem starts with `stem`. Growth means progress.
fn output_progress(dir: &Path, stem: &str) -> (u64, u64) {
    let mut count = 0u64;
    let mut bytes = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            let matches = p
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|n| n.starts_with(stem))
                .unwrap_or(false);
            if matches {
                count += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (count, bytes)
}

/// Run one whole-document `pdfimages <fmt> -p` pass into `prefix`. `-p` embeds
/// the 1-based page number in each output filename (`prefix-<page>-<n>.<ext>`),
/// so every page's images can be grouped and OCR'd without re-parsing the PDF.
///
/// `stall` is a progress watchdog, not a wall-clock budget: the process may run
/// as long as it needs, and is only killed after `stall` elapses with **no new
/// output** (a wedged/corrupt file). `None` disables the watchdog.
fn run_pdfimages_doc(
    pdf_path: &Path,
    fmt: &str,
    prefix: &Path,
    stall: Option<Duration>,
) -> Result<()> {
    let bin = pdfimages_path()
        .ok_or_else(|| anyhow::anyhow!("pdfimages not available. Install poppler-utils."))?;
    let mut cmd = crate::process::new(bin);
    cmd.args([fmt, "-p"]).arg(pdf_path).arg(prefix);
    cmd.stderr(Stdio::null());

    let dir = prefix
        .parent()
        .ok_or_else(|| anyhow::anyhow!("bad pdfimages output prefix"))?;
    let stem = prefix.file_name().and_then(|s| s.to_str()).unwrap_or("img");

    let mut child = cmd.spawn().context("failed to spawn pdfimages")?;
    let mut last_progress = Instant::now();
    let mut last = output_progress(dir, stem);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                let now = output_progress(dir, stem);
                if now != last {
                    last = now;
                    last_progress = Instant::now();
                } else if let Some(stall) = stall {
                    if last_progress.elapsed() >= stall {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(anyhow::anyhow!(
                            "pdfimages stalled: no output progress for {stall:?}"
                        ));
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(anyhow::anyhow!("pdfimages failed: {e}")),
        }
    };
    if !status.success() {
        return Err(anyhow::anyhow!("pdfimages failed"));
    }
    Ok(())
}

/// Image extensions `image::open` may be asked to decode across pdfimages'
/// output formats.
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "ppm", "pgm", "pbm", "pam", "jp2", "j2k", "tif", "tiff", "bmp",
];

/// True when a page's extracted images are a single JPEG — the only shape that
/// the fast native path can hand to the OCR engine unchanged.
fn is_single_jpeg_file(images: &[(PathBuf, u64)]) -> bool {
    images.len() == 1
        && images[0]
            .0
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))
}

/// Group `pdfimages -p` output by page, keeping **all** decodable images on
/// each 1-based page as `(path, area_px)`, sorted largest-first.
///
/// Keeping every image matters: some pages carry the real text in a small
/// CCITT/JBIG2 stencil while the full-page background is nearly blank, so
/// "largest image only" silently drops the body.
fn collect_page_images(dir: &Path, page_count: usize) -> Result<Vec<Vec<(PathBuf, u64)>>> {
    let mut per_page: Vec<Vec<(PathBuf, u64)>> = vec![Vec::new(); page_count];
    for entry in std::fs::read_dir(dir)?.flatten() {
        let p = entry.path();
        let is_img = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false);
        if !is_img {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // `img-<page>-<n>` → rsplitn yields [n, page, head].
        let mut parts = stem.rsplitn(3, '-');
        let _n = parts.next();
        let Some(page) = parts.next().and_then(|s| s.parse::<usize>().ok()) else {
            continue;
        };
        if page == 0 || page > page_count {
            continue;
        }
        let Ok((w, h)) = image::image_dimensions(&p) else {
            continue;
        };
        per_page[page - 1].push((p, w as u64 * h as u64));
    }
    for images in &mut per_page {
        images.sort_by(|a, b| b.1.cmp(&a.1));
    }
    Ok(per_page)
}

#[cfg(test)]
mod tests {
    /// 有界窗口：每页恰好派发一次、结果按输入顺序对齐、并发不超过窗口。
    #[test]
    fn windowed_pages_dispatch_each_page_once_and_respect_window() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pages: Vec<u32> = (1..=20).collect();
        let calls = AtomicUsize::new(0);
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);

        let outcome = run_pages_windowed(&pages, None, None, || false, |page| {
            calls.fetch_add(1, Ordering::SeqCst);
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(15));
            live.fetch_sub(1, Ordering::SeqCst);
            Some(format!("page-{page}"))
        });

        assert_eq!(
            calls.load(Ordering::SeqCst),
            20,
            "每页必须恰好派发一次"
        );
        assert_eq!(outcome.attempted, 20);
        assert!(!outcome.stalled && !outcome.cancelled);
        assert_eq!(outcome.texts.len(), 20, "结果槽必须与输入页对齐");
        assert_eq!(outcome.texts[0].as_deref(), Some("page-1"));
        assert_eq!(outcome.texts[19].as_deref(), Some("page-20"));
        assert!(
            peak.load(Ordering::SeqCst) <= PAGE_OCR_WINDOW,
            "并发峰值 {} 必须 ≤ PAGE_OCR_WINDOW ({PAGE_OCR_WINDOW})",
            peak.load(Ordering::SeqCst)
        );
        assert!(
            peak.load(Ordering::SeqCst) > 1,
            "窗口应当真的并行，实测峰值 {}",
            peak.load(Ordering::SeqCst)
        );
        // 上报的峰值并发要能被日志归因用（不能恒为 0/1）。
        assert!(
            outcome.peak_concurrency > 1 && outcome.peak_concurrency <= PAGE_OCR_WINDOW,
            "上报的峰值并发 {} 应当 >1 且 ≤ 窗口 {PAGE_OCR_WINDOW}",
            outcome.peak_concurrency
        );
    }

    /// 一页都出不了文字时（`give_up_after`）尽早停，别把整本扫描件渲染一遍。
    #[test]
    fn windowed_pages_gives_up_when_nothing_succeeds() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pages: Vec<u32> = (1..=50).collect();
        let calls = AtomicUsize::new(0);
        let outcome = run_pages_windowed(&pages, None, Some(5), || false, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            None
        });

        assert!(outcome.stalled, "全部无文字时必须触发放弃");
        assert!(outcome.texts.iter().all(|t| t.is_none()));
        let n = calls.load(Ordering::SeqCst);
        assert!(n >= 5, "至少尝试 5 页才放弃，实测 {n}");
        assert!(n <= 5 + PAGE_OCR_WINDOW, "放弃后不应再多派发，实测 {n}");
    }

    /// 连续 `stall × window` 内一页都没完成（真卡死）→ 停止派发，已产出结果保留。
    /// 注意判定的是**进展**，不是单页耗时：并发下单页变慢不算卡死。
    #[test]
    fn windowed_pages_stall_stops_dispatch_but_keeps_results() {
        let pages: Vec<u32> = (1..=40).collect();
        let outcome = run_pages_windowed(&pages, Some(Duration::from_millis(10)), None, || false, |page| {
            // 每一页都超过 10ms 预算：整本卡死的情形。
            std::thread::sleep(Duration::from_millis(50));
            Some(format!("page-{page}"))
        });

        assert!(outcome.stalled, "超预算必须触发 stall");
        assert!(
            outcome.attempted >= PAGE_OCR_WINDOW,
            "第一窗口应当已经派发出去，实测 {}",
            outcome.attempted
        );
        assert!(
            outcome.attempted <= PAGE_OCR_WINDOW * 2,
            "stall 后只应跑完在飞的页，实测 {}",
            outcome.attempted
        );
        assert_eq!(outcome.texts[0].as_deref(), Some("page-1"), "在飞页结果要保留");
    }

    use super::*;

    fn write_png(path: &Path, w: u32, h: u32) {
        image::RgbImage::new(w, h).save(path).unwrap();
    }

    #[test]
    fn output_progress_counts_and_sums_matching_files() {
        let tmp = TempDir::new("ls_progress_test").unwrap();
        assert_eq!(output_progress(tmp.path(), "img"), (0, 0));
        std::fs::write(tmp.path().join("img-001-000.png"), b"abcd").unwrap();
        std::fs::write(tmp.path().join("img-002-000.png"), b"abcdefgh").unwrap();
        // Non-matching entries are ignored.
        std::fs::write(tmp.path().join("other.txt"), b"xxxxxxxxxx").unwrap();
        assert_eq!(output_progress(tmp.path(), "img"), (2, 12));
    }

    #[test]
    fn merge_page_texts_splices_ocr_into_text_layer() {
        let tl = vec!["页1原生".to_string(), "页2原生".to_string()];
        let mut ocr = HashMap::new();
        ocr.insert(1usize, "页2 OCR".to_string());
        assert_eq!(merge_page_texts(&tl, &ocr), "页1原生\n页2 OCR");
    }

    /// 回归：强制 OCR 路径传的是**空** text_layer。旧实现只遍历 text_layer，
    /// 会把所有 OCR 结果丢掉、返回空串（实测在 145 页卷宗上丢了 142 页文字）。
    #[test]
    fn merge_page_texts_handles_empty_text_layer() {
        let mut ocr = HashMap::new();
        for i in 0..142usize {
            ocr.insert(i, format!("第{i}页OCR"));
        }
        let merged = merge_page_texts(&[], &ocr);
        assert!(merged.starts_with("第0页OCR"), "空 text_layer 也必须保留 OCR 结果");
        assert!(merged.contains("第141页OCR"), "最后一页不能丢");
        assert_eq!(merged.lines().count(), 142, "页数应等于 OCR 命中页数");
    }

    #[test]
    fn merge_page_texts_prefers_ocr_and_keeps_unmatched_pages() {
        // OCR 只覆盖部分页时，其余页保留原生文本；页数以两者最大值为准
        let tl = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut ocr = HashMap::new();
        ocr.insert(1usize, "B".to_string());
        ocr.insert(3usize, "D".to_string()); // 超出 text_layer 页数
        let merged = merge_page_texts(&tl, &ocr);
        assert_eq!(merged, "a\nB\nc\nD");
    }

    #[test]
    fn merge_page_texts_empty_inputs() {
        assert_eq!(merge_page_texts(&[], &HashMap::new()), "");
    }

    #[test]
    fn ocr_gate_rejects_rotated_page_garbage() {
        // 旋转扫描件的 pdfimages OCR 输出长这样：每个字一行、没有词。
        // 旧门槛只看 len>100，会被它骗过 → 必须判为不可用。
        let garbage = "0\n冈\n半\n叫\n国\n悔\n溲\n叫\n世\n瞓\n绊 7\n屉\n专\n奬\n瞓\n甲\n0 、\n0\n冈\n叫\n跹\n叵\n0\n0\n叫\n逭\n《 0\n典\n叵\n叫\n岬 鋈\n回 .\n";
        assert!(ocr_text_is_unusable(garbage));
        // 正常的 OCR 判决书正文（多字成行）必须放行。
        let normal = "上海市徐汇区人民法院\n民事判决书\n(2019)沪0104民初16644号\n原告:上海尊信科技服务(集团)有限责任公司,住所地上海市闵行区七莘路1855号第1幢208室。\n法定代表人:卢前荣,该公司董事长。\n被告:上海典欧实业有限公司。\n本院认为,涉案协议系双方就原告代理被告申请注册商标事宜达成的合意,合法有效,对原被告均有约束力。\n";
        assert!(!ocr_text_is_unusable(normal));
    }

    #[test]
    fn ocr_gate_rejects_empty_and_short() {
        assert!(ocr_text_is_unusable(""));
        assert!(ocr_text_is_unusable("\n\n   \n"));
    }

    #[test]
    fn collect_page_images_keeps_all_images_and_sorts_largest_first() {
        let tmp = TempDir::new("ls_collect_test").unwrap();
        // Page 1 carries two images — both must be kept, largest first.
        write_png(&tmp.path().join("img-001-000.png"), 50, 50);
        write_png(&tmp.path().join("img-001-001.png"), 400, 300);
        // Page 2 has no image (only a non-image file).
        std::fs::write(tmp.path().join("img-002-000.txt"), b"x").unwrap();
        // Page 3 has one image.
        write_png(&tmp.path().join("img-003-000.png"), 200, 200);
        // Out-of-range page must be ignored.
        write_png(&tmp.path().join("img-009-000.png"), 100, 100);

        let grouped = collect_page_images(tmp.path(), 3).unwrap();
        assert_eq!(grouped.len(), 3);
        assert_eq!(grouped[0].len(), 2, "page 1 must keep both images");
        assert_eq!(grouped[0][0].1, 400 * 300, "largest image first");
        assert!(grouped[1].is_empty(), "page 2 has no image");
        assert_eq!(grouped[2].len(), 1);
        assert_eq!(grouped[2][0].1, 200 * 200);
    }

    /// 取消检查在派发新页之前生效（判定为真 → 一页都不派发）。
    #[test]
    fn windowed_pages_honours_cancellation() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pages: Vec<u32> = (1..=10).collect();
        let calls = AtomicUsize::new(0);
        let outcome = run_pages_windowed(&pages, None, None, || true, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Some("x".to_string())
        });

        assert!(outcome.cancelled, "必须报告已取消");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "取消后不应派发任何页");
        assert!(outcome.texts.iter().all(|t| t.is_none()));
        assert_eq!(outcome.attempted, 0);
    }
}
