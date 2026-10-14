//! Memory-pressure aware load adaptation ("优雅降级 / 升级").
//!
//! Why this exists: the OCR pipeline is the memory hog — whole-PDF object
//! graphs, decoded page bitmaps (the 2500px renders are ~20–30MB each) and one
//! ONNX arena per OCR engine. Measured on a 31GB machine: 7.1GB resident /
//! 8.9GB peak while three big scans were re-indexed. On Windows the failure
//! modes are both bad:
//!
//! * physical memory runs out → the page file takes over → no error, but every
//!   page OCR goes from seconds to minutes and the whole machine crawls;
//! * the *commit limit* runs out → `VirtualAlloc` fails → Rust's allocation
//!   error path calls `handle_alloc_error` → **abort()**. That is not a panic,
//!   so `catch_unwind` in `extractor/pdf.rs` cannot contain it: the process
//!   dies mid-index.
//!
//! So instead of letting it get there, a supervisor samples memory every few
//! seconds and moves the pipeline through four levels:
//!
//! ```text
//! Normal ──(压力升高)──▶ Soft ──▶ Hard ──▶ Paused
//!        ◀──(连续 N 次平静，逐级回升)──
//! ```
//!
//! * **Normal** – configured maximum concurrency (today's behaviour);
//! * **Soft / Hard** – fewer concurrent page-OCR calls, a smaller page window
//!   and fewer documents in flight (all from settings);
//! * **Paused** – no new pages are dispatched and new documents wait until
//!   pressure drops; work already in flight finishes normally (graceful).
//!
//! Image resolution is deliberately **not** touched: degrading it would trade
//! OCR quality for memory behind the user's back.
//!
//! Every transition is logged as `[LOAD] …` so a slow run is explainable after
//! the fact, and the current level is exposed through the index status API/UI.

use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Ordered worst-last so comparisons read naturally (`target > current`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Normal = 0,
    Soft = 1,
    Hard = 2,
    Paused = 3,
}

impl Level {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Level::Soft,
            2 => Level::Hard,
            _ if v >= 3 => Level::Paused,
            _ => Level::Normal,
        }
    }

    /// Stable identifier used by the IPC/Web status payloads and the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Normal => "normal",
            Level::Soft => "soft",
            Level::Hard => "hard",
            Level::Paused => "paused",
        }
    }

    /// Human readable (logs, UI tooltips).
    pub fn as_label(self) -> &'static str {
        match self {
            Level::Normal => "正常",
            Level::Soft => "软降级",
            Level::Hard => "硬降级",
            Level::Paused => "暂停派发",
        }
    }

    fn better(self) -> Self {
        match self {
            Level::Normal => Level::Normal,
            Level::Soft => Level::Normal,
            Level::Hard => Level::Soft,
            Level::Paused => Level::Hard,
        }
    }
}

/// One memory sample. Kept as plain numbers so the decision logic is pure and
/// unit-testable without touching the OS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// This process' resident/working set, MB.
    pub process_ws_mb: u64,
    /// Whole-system memory in use (RAM + page file), percent of total.
    pub system_used_pct: u64,
    /// Installed physical memory, MB — the process thresholds are relative to it
    /// (an absolute MB budget would throttle a 32GB box that is perfectly fine,
    /// or be unreachable on a 16GB one).
    pub total_mb: u64,
}

/// User-tunable thresholds and concurrency floors (all from `app_settings`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadSettings {
    pub enabled: bool,
    /// Process working-set thresholds, **percent of installed RAM**:
    /// soft → hard → pause. (Measured: 3 big scans in parallel peak at ~10GB on
    /// a 31GB box while 12GB stays free — an absolute MB budget would degrade a
    /// healthy machine, while a percentage scales with the host.)
    pub process_soft_pct: u64,
    pub process_hard_pct: u64,
    pub process_pause_pct: u64,
    /// System memory-in-use thresholds (percent): soft → hard → pause.
    pub system_soft_pct: u64,
    pub system_hard_pct: u64,
    pub system_pause_pct: u64,
    /// Concurrency at Soft (page window / OCR permits / documents in flight).
    pub soft_page_window: usize,
    pub soft_ocr_permits: usize,
    pub soft_doc_permits: usize,
    /// Concurrency at Hard.
    pub hard_page_window: usize,
    pub hard_ocr_permits: usize,
    pub hard_doc_permits: usize,
    /// Seconds between samples.
    pub sample_secs: u64,
    /// Consecutive calm samples required before stepping one level back up.
    pub upgrade_samples: u32,
    /// Hysteresis band: a sample must be this many percentage points *better*
    /// than the current level's threshold to count as "calm" (prevents the
    /// level from flapping around a threshold — measured 59 switches in one run).
    pub recovery_margin_pct: u64,
}

impl Default for LoadSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            process_soft_pct: 40,
            process_hard_pct: 55,
            process_pause_pct: 70,
            system_soft_pct: 85,
            system_hard_pct: 90,
            system_pause_pct: 95,
            soft_page_window: 3,
            soft_ocr_permits: 8,
            soft_doc_permits: 6,
            hard_page_window: 2,
            hard_ocr_permits: 6,
            hard_doc_permits: 4,
            // 回滞带：只有样本明显优于当前档阈值（低 margin 个百分点）才允许升档，
            // 否则会像实测那样在阈值上下反复切换（本轮 59 次）。
            recovery_margin_pct: 5,
            sample_secs: 5,
            upgrade_samples: 12,
        }
    }
}

/// Read a `u64` from a settings map, falling back to a default.
fn num(map: &std::collections::HashMap<String, String>, key: &str, dflt: u64) -> u64 {
    map.get(key)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(dflt)
}

fn flag(map: &std::collections::HashMap<String, String>, key: &str, dflt: bool) -> bool {
    map.get(key)
        .map(|v| !matches!(v.trim(), "0" | "false" | "off" | "no"))
        .unwrap_or(dflt)
}

impl LoadSettings {
    /// Parse from `app_settings` (DB is the source of truth, so a change takes
    /// effect on the next sample without restarting the app).
    pub fn from_map(map: &std::collections::HashMap<String, String>) -> Self {
        let d = Self::default();
        let mut s = Self {
            enabled: flag(map, "adaptive_load_enabled", d.enabled),
            process_soft_pct: num(map, "load_process_soft_pct", d.process_soft_pct),
            process_hard_pct: num(map, "load_process_hard_pct", d.process_hard_pct),
            process_pause_pct: num(map, "load_process_pause_pct", d.process_pause_pct),
            system_soft_pct: num(map, "load_system_soft_pct", d.system_soft_pct),
            system_hard_pct: num(map, "load_system_hard_pct", d.system_hard_pct),
            system_pause_pct: num(map, "load_system_pause_pct", d.system_pause_pct),
            soft_page_window: num(map, "load_soft_page_window", d.soft_page_window as u64) as usize,
            soft_ocr_permits: num(map, "load_soft_ocr_permits", d.soft_ocr_permits as u64) as usize,
            soft_doc_permits: num(map, "load_soft_doc_permits", d.soft_doc_permits as u64) as usize,
            hard_page_window: num(map, "load_hard_page_window", d.hard_page_window as u64) as usize,
            hard_ocr_permits: num(map, "load_hard_ocr_permits", d.hard_ocr_permits as u64) as usize,
            hard_doc_permits: num(map, "load_hard_doc_permits", d.hard_doc_permits as u64) as usize,
            sample_secs: num(map, "load_sample_secs", d.sample_secs).clamp(1, 600),
            upgrade_samples: num(map, "load_upgrade_samples", d.upgrade_samples as u64).clamp(1, 720) as u32,
            recovery_margin_pct: num(map, "load_recovery_margin_pct", d.recovery_margin_pct).min(30),
        };
        // Keep the ladder monotonic even if the user typed nonsense.
        s.process_hard_pct = s.process_hard_pct.max(s.process_soft_pct);
        s.process_pause_pct = s.process_pause_pct.max(s.process_hard_pct);
        s.process_soft_pct = s.process_soft_pct.min(95);
        s.process_hard_pct = s.process_hard_pct.min(96);
        s.process_pause_pct = s.process_pause_pct.min(97);
        s.system_hard_pct = s.system_hard_pct.max(s.system_soft_pct);
        s.system_pause_pct = s.system_pause_pct.max(s.system_hard_pct);
        s.soft_page_window = s.soft_page_window.clamp(1, MAX_PAGE_WINDOW);
        s.hard_page_window = s.hard_page_window.clamp(1, s.soft_page_window);
        s.soft_ocr_permits = s.soft_ocr_permits.clamp(1, MAX_OCR_PERMITS);
        s.hard_ocr_permits = s.hard_ocr_permits.clamp(1, s.soft_ocr_permits);
        s.soft_doc_permits = s.soft_doc_permits.clamp(1, MAX_DOC_PERMITS);
        s.hard_doc_permits = s.hard_doc_permits.clamp(1, s.soft_doc_permits);
        s
    }

    /// Which level this sample implies (worst signal wins).
    pub fn classify(&self, sample: &Sample) -> Level {
        self.classify_with_margin(sample, 0)
    }

    /// Classify with a hysteresis margin: every threshold is relaxed by `margin`
    /// percentage points, so a sample must be *clearly* below a threshold to be
    /// considered calm. Used for the upgrade decision only.
    pub fn classify_with_margin(&self, sample: &Sample, margin: u64) -> Level {
        if !self.enabled {
            return Level::Normal;
        }
        let total = sample.total_mb.max(1);
        let pct_of = |p: u64| total.saturating_mul(p.saturating_sub(margin).max(1)) / 100;
        let sys_of = |p: u64| p.saturating_sub(margin).max(1);
        let proc = if sample.process_ws_mb >= pct_of(self.process_pause_pct) {
            Some(Level::Paused)
        } else if sample.process_ws_mb >= pct_of(self.process_hard_pct) {
            Some(Level::Hard)
        } else if sample.process_ws_mb >= pct_of(self.process_soft_pct) {
            Some(Level::Soft)
        } else {
            None
        };
        let sys = if sample.system_used_pct >= sys_of(self.system_pause_pct) {
            Some(Level::Paused)
        } else if sample.system_used_pct >= sys_of(self.system_hard_pct) {
            Some(Level::Hard)
        } else if sample.system_used_pct >= sys_of(self.system_soft_pct) {
            Some(Level::Soft)
        } else {
            None
        };
        match (proc, sys) {
            (Some(a), Some(b)) => a.max(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => Level::Normal,
        }
    }
}

/// Concurrency allowed at a given level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Pages OCR'd in parallel inside one document.
    pub page_window: usize,
    /// Concurrent OCR inference calls process-wide.
    pub ocr_permits: usize,
    /// Documents extracted at the same time (`0` = unlimited, Normal only).
    pub doc_permits: usize,
}

impl LoadSettings {
    pub fn limits_for(&self, level: Level) -> Limits {
        match level {
            Level::Normal => Limits {
                page_window: page_window_default(),
                ocr_permits: ocr_permits_default(),
                doc_permits: 0,
            },
            Level::Soft => Limits {
                page_window: self.soft_page_window,
                ocr_permits: self.soft_ocr_permits,
                doc_permits: self.soft_doc_permits,
            },
            Level::Hard => Limits {
                page_window: self.hard_page_window,
                ocr_permits: self.hard_ocr_permits,
                doc_permits: self.hard_doc_permits,
            },
            Level::Paused => Limits {
                page_window: 0,
                ocr_permits: 0,
                doc_permits: 0,
            },
        }
    }
}

/// Default (= current, pre-adaptation) page window: `PAGE_OCR_WINDOW`.
pub fn page_window_default() -> usize {
    crate::extractor::pdf::default_page_window()
}

/// Default OCR concurrency: the engine pool size (one engine per core, max 8).
///
/// Must mirror `extractor::paddleocr`'s `available_parallelism().clamp(1, 8)` —
/// it used to clamp to 16, so the status API/logs advertised 12 permits on a
/// 12-core box while the pool only ever had 8 engines.
pub fn ocr_permits_default() -> usize {
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(2)
        .clamp(1, 8)
}

/// Absolute ceilings, so a bad setting cannot make the app explode.
pub const MAX_PAGE_WINDOW: usize = 16;
pub const MAX_OCR_PERMITS: usize = 16;
pub const MAX_DOC_PERMITS: usize = 64;

/// Advance the controller by one sample: degrade immediately, recover slowly.
///
/// Returns the new level and the new "consecutive calm samples" streak. Pure so
/// the hysteresis can be tested without threads.
pub fn next_level(
    current: Level,
    sample: &Sample,
    settings: &LoadSettings,
    calm_streak: u32,
) -> (Level, u32) {
    let target = settings.classify(sample);
    if target > current {
        // Degrade at once: memory pressure does not wait for a streak.
        return (target, 0);
    }
    if target < current {
        // Calm again: only step back up one level at a time, and only after the
        // streak. The **hysteresis band** matters here: the sample must be
        // clearly below the current level's threshold (by `recovery_margin_pct`)
        // — otherwise a value hovering exactly at the threshold flips the level
        // back and forth (measured: 59 switches in one run, each one halving the
        // page window and OCR permits).
        let relaxed = settings.classify_with_margin(sample, settings.recovery_margin_pct);
        if relaxed >= current {
            return (current, 0);
        }
        let streak = calm_streak + 1;
        if streak >= settings.upgrade_samples {
            return (current.better(), 0);
        }
        return (current, streak);
    }
    (current, 0)
}

// ───────────────────────────── runtime gate ─────────────────────────────

/// Test/ops override: `LINK_SEARCHER_LOAD_FORCE=soft|hard|paused|normal`.
const ENV_FORCE: &str = "LINK_SEARCHER_LOAD_FORCE";

fn forced_level() -> Option<Level> {
    let raw = std::env::var(ENV_FORCE).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "normal" | "0" => Some(Level::Normal),
        "soft" | "1" => Some(Level::Soft),
        "hard" | "2" => Some(Level::Hard),
        "paused" | "pause" | "3" => Some(Level::Paused),
        _ => None,
    }
}

/// Which permit a caller wants from [`Gate::acquire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ocr,
    Doc,
}

struct Gate {
    level: AtomicU8,
    page_window: AtomicUsize,
    ocr_max: AtomicUsize,
    doc_max: AtomicUsize,
    ocr_in_use: AtomicUsize,
    doc_in_use: AtomicUsize,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Gate {
    fn new() -> Self {
        let s = LoadSettings::default();
        let n = s.limits_for(Level::Normal);
        Self {
            level: AtomicU8::new(Level::Normal as u8),
            page_window: AtomicUsize::new(n.page_window),
            ocr_max: AtomicUsize::new(n.ocr_permits),
            doc_max: AtomicUsize::new(n.doc_permits),
            ocr_in_use: AtomicUsize::new(0),
            doc_in_use: AtomicUsize::new(0),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    fn apply(&self, level: Level, settings: &LoadSettings) {
        let limits = settings.limits_for(level);
        self.level.store(level as u8, Ordering::Relaxed);
        self.page_window.store(limits.page_window, Ordering::Relaxed);
        self.ocr_max.store(limits.ocr_permits, Ordering::Relaxed);
        self.doc_max.store(limits.doc_permits, Ordering::Relaxed);
        // Wake anyone parked in `acquire` so a change is felt immediately.
        let _guard = self.lock.lock();
        self.cv.notify_all();
    }

    fn acquire(&self, kind: Kind) -> Permit<'_> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let max = match kind {
                Kind::Ocr => self.ocr_max.load(Ordering::Relaxed),
                Kind::Doc => self.doc_max.load(Ordering::Relaxed),
            };
            let in_use = match kind {
                Kind::Ocr => &self.ocr_in_use,
                Kind::Doc => &self.doc_in_use,
            };
            // `max == 0` means either "Paused" (OCR permits) or "unlimited"
            // (document permits at Normal) — tell them apart by level.
            let paused = Level::from_u8(self.level.load(Ordering::Relaxed)) == Level::Paused;
            if max == 0 && !paused {
                return Permit { gate: self, kind };
            }
            if max > 0 && in_use.load(Ordering::Relaxed) < max {
                in_use.fetch_add(1, Ordering::Relaxed);
                return Permit { gate: self, kind };
            }
            // 等太久必须放行：阻塞式闸门曾把一个"卡住的文档"放大成整条流水线停摆
            // （2026-10-14 实测：Hard 档 4 个文档许可被占满 → 25 分钟零进展）。
            // 放行时同样计数（drop 时会减回去），并把这件事记进日志。
            if Instant::now() >= deadline {
                in_use.fetch_add(1, Ordering::Relaxed);
                log::warn!("[LOAD] 并发闸门等待超过 30s → 强制放行（防止死锁，闸门被卡住时不应拖死全局）");
                return Permit { gate: self, kind };
            }
            let (g, _timeout) = self
                .cv
                .wait_timeout(guard, Duration::from_millis(200))
                .unwrap_or_else(|e| e.into_inner());
            guard = g;
        }
    }
}

/// RAII guard: releases the slot even if the caller panics.
pub struct Permit<'a> {
    gate: &'a Gate,
    kind: Kind,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let slot = match self.kind {
            Kind::Ocr => &self.gate.ocr_in_use,
            Kind::Doc => &self.gate.doc_in_use,
        };
        slot.fetch_sub(1, Ordering::Relaxed);
        let _guard = self.gate.lock.lock();
        self.gate.cv.notify_all();
    }
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(Gate::new)
}

/// Pages OCR'd in parallel inside one document (0 = paused, dispatch nothing).
pub fn page_window() -> usize {
    if let Some(l) = forced_level() {
        return LoadSettings::default().limits_for(l).page_window;
    }
    gate().page_window.load(Ordering::Relaxed)
}

/// True when memory pressure is high enough that new pages/documents must wait.
pub fn is_paused() -> bool {
    if let Some(l) = forced_level() {
        return l == Level::Paused;
    }
    Level::from_u8(gate().level.load(Ordering::Relaxed)) == Level::Paused
}

pub fn level() -> Level {
    if let Some(l) = forced_level() {
        return l;
    }
    Level::from_u8(gate().level.load(Ordering::Relaxed))
}

/// Current effective limits (for the status API / UI).
pub fn current_limits() -> Limits {
    let l = gate();
    Limits {
        page_window: l.page_window.load(Ordering::Relaxed),
        ocr_permits: l.ocr_max.load(Ordering::Relaxed),
        doc_permits: l.doc_max.load(Ordering::Relaxed),
    }
}

/// Number of page-OCR passes currently running (one per document).
///
/// 2026-10-15 实测：`PAGE_OCR_WINDOW` 是**每文档**上限，而引擎闸门是 8 —— 文档数 ≥2 时
/// 能打满（24~44 页/分），只剩 1 篇时只用 4 个槽（13~20 页/分，**腰斩**）。批尾与
/// 小批（如失败回收的 124 个文件）全程都处在"文档数少"的状态，因此整轮被拖慢。
static ACTIVE_PASSES: AtomicUsize = AtomicUsize::new(0);

/// RAII：进入一次逐页 OCR pass（每文档一次），退出自动递减。
pub struct PassGuard;

impl Drop for PassGuard {
    fn drop(&mut self) {
        ACTIVE_PASSES.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn begin_page_pass() -> PassGuard {
    ACTIVE_PASSES.fetch_add(1, Ordering::Relaxed);
    PassGuard
}

/// 纯函数：把全局页预算（= 引擎闸门）按在飞 pass 数均分，下限为配置的页窗口。
/// 文档少时自动放大，单文档也能吃满闸门（内存上限仍是 闸门 × 单页位图 ≈ 200MB）。
fn window_for(base: usize, gate: usize, passes: usize) -> usize {
    if base == 0 {
        return 0; // 暂停档：不派发新页
    }
    let gate = gate.max(1);
    (gate / passes.max(1)).max(base).min(gate)
}

/// 本次 pass 可用的页窗口（替代直接读 `page_window()`）。
pub fn page_window_for_pass() -> usize {
    window_for(
        page_window(),
        ocr_permits_default(),
        ACTIVE_PASSES.load(Ordering::Relaxed),
    )
}

/// Take one OCR slot (one page-image inference). Blocks while `Paused`.
pub fn acquire_ocr() -> Permit<'static> {
    gate().acquire(Kind::Ocr)
}

/// Take one document slot (one file being extracted). Blocks while throttled.
pub fn acquire_doc() -> Permit<'static> {
    gate().acquire(Kind::Doc)
}

/// Monotonic counter of "a page pass stopped early because of memory pressure".
///
/// A boolean flag would be racy across the extraction workers (thread A could
/// consume thread B's flag), so callers snapshot this counter before extracting
/// and compare afterwards: any change means some pass was truncated while ours
/// was in flight. The pressure is process-wide, so an overlapping doc really was
/// truncated too — worst case a short doc is retried once.
static PAUSE_EPOCH: AtomicU64 = AtomicU64::new(0);

pub fn pause_epoch() -> u64 {
    PAUSE_EPOCH.load(Ordering::Relaxed)
}

/// Called by the page dispatcher when it stops because of `Paused`.
pub fn note_paused_in_pass() {
    PAUSE_EPOCH.fetch_add(1, Ordering::Relaxed);
}

/// True if any page pass was truncated by memory pressure since `epoch0`.
pub fn truncated_since(epoch0: u64) -> bool {
    pause_epoch() != epoch0
}

/// Apply a level choice (used by the supervisor and by tests).
pub fn apply_level(level: Level, settings: &LoadSettings) {
    gate().apply(level, settings);
}

/// Identify the current process' working set and the system's memory use.
pub fn sample() -> Sample {
    use sysinfo::{ProcessesToUpdate, System};

    let mut sys = System::new();
    sys.refresh_memory();
    let total = sys.total_memory().max(1);
    let used = sys.used_memory().min(total);
    let system_used_pct = (used.saturating_mul(100) / total).min(100);

    let pid = sysinfo::get_current_pid().ok();
    let process_ws_mb = match pid {
        Some(pid) => {
            sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
            sys.process(pid).map(|p| p.memory() / 1024 / 1024).unwrap_or(0)
        }
        None => 0,
    };
    Sample {
        process_ws_mb,
        system_used_pct,
        total_mb: total / 1024 / 1024,
    }
}

/// Load the settings row map from the DB (missing keys fall back to defaults).
fn read_settings(conn: &rusqlite::Connection) -> LoadSettings {
    let mut map = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM app_settings") {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
        }
    }
    LoadSettings::from_map(&map)
}

/// Start the sampler thread. Runs for the lifetime of the process; every change
/// is logged, every sample is cheap (a couple of syscalls).
pub fn start_supervisor(db: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>) {
    std::thread::Builder::new()
        .name("load-supervisor".into())
        .spawn(move || {
            let mut current = Level::Normal;
            let mut streak: u32 = 0;
            let mut announced: Option<(Level, Limits)> = None;
            let mut settings = LoadSettings::default();
            let mut ticks: u32 = 0;
            loop {
                // 设置每 ~60s 重读一次（12 × 采样间隔）：实测每 5s 读一次会与提取
                // 流水线抢连接池（本轮 47 次 "读取设置失败（连接池）"）。压力采样仍每轮都做。
                if ticks.is_multiple_of(12) {
                    match db.get() {
                        Ok(conn) => {
                            settings = read_settings(&conn);
                        }
                        Err(e) => log::warn!("[LOAD] 读取设置失败（沿用上次的值）: {e}"),
                    }
                }
                ticks = ticks.wrapping_add(1);

                if let Some(forced) = forced_level() {
                    // Manual override (tests / ops): keep the gate pinned there.
                    if announced.map(|(l, _)| l) != Some(forced) {
                        apply_level(forced, &settings);
                        log::warn!(
                            "[LOAD] 强制档位(环境变量 {ENV_FORCE}={}): {} → 页窗口 {}, OCR 许可 {}, 并发文档 {}",
                            forced.as_str(),
                            forced.as_label(),
                            settings.limits_for(forced).page_window,
                            settings.limits_for(forced).ocr_permits,
                            settings.limits_for(forced).doc_permits,
                        );
                        announced = Some((forced, settings.limits_for(forced)));
                    }
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }

                let s = sample();
                let (next, new_streak) = next_level(current, &s, &settings, streak);
                streak = new_streak;
                if next != current {
                    let before = settings.limits_for(current);
                    let after = settings.limits_for(next);
                    apply_level(next, &settings);
                    log::info!(
                        "[LOAD] 档位 {} → {}（本进程 {:.1}GB / 物理 {:.0}GB，系统已用 {}%；阈值 进程 {}/{}/{}%，系统 {}/{}/{}%）\
                         ：页窗口 {}→{}，OCR 许可 {}→{}，并发文档 {}→{}",
                        current.as_label(),
                        next.as_label(),
                        s.process_ws_mb as f64 / 1024.0,
                        s.total_mb as f64 / 1024.0,
                        s.system_used_pct,
                        settings.process_soft_pct,
                        settings.process_hard_pct,
                        settings.process_pause_pct,
                        settings.system_soft_pct,
                        settings.system_hard_pct,
                        settings.system_pause_pct,
                        page_window_default(),
                        after.page_window,
                        ocr_permits_default(),
                        after.ocr_permits,
                        doc_label(before.doc_permits),
                        doc_label(after.doc_permits),
                    );
                    current = next;
                    announced = Some((next, after));
                } else if announced.is_none() {
                    apply_level(current, &settings);
                    log::info!(
                        "[LOAD] 自适应并发已启用：档位 {}，页窗口 {}，OCR 许可 {}，并发文档 {}（采样 {}s）",
                        current.as_label(),
                        settings.limits_for(current).page_window,
                        settings.limits_for(current).ocr_permits,
                        doc_label(settings.limits_for(current).doc_permits),
                        settings.sample_secs,
                    );
                    announced = Some((current, settings.limits_for(current)));
                } else {
                    // Settings may have changed at runtime → keep the gate in sync.
                    apply_level(current, &settings);
                }
                std::thread::sleep(Duration::from_secs(settings.sample_secs.max(1)));
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| log::warn!("[LOAD] 采样线程启动失败: {e}"));
}

fn doc_label(n: usize) -> String {
    if n == 0 { "不限".to_string() } else { n.to_string() }
}

/// Wait until the current level differs from `from` (test helper) — bounded.
#[cfg(test)]
fn wait_for_level_change(from: Level, timeout: Duration) -> Option<Level> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let l = level();
        if l != from {
            return Some(l);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> LoadSettings {
        LoadSettings::default()
    }

    /// 32GB 机器 → 进程阈值 40/55/70% = 13,107 / 18,022 / 22,937MB
    fn sample(ws_mb: u64, pct: u64) -> Sample {
        Sample { process_ws_mb: ws_mb, system_used_pct: pct, total_mb: 32768 }
    }

    #[test]
    fn classify_is_normal_when_everything_is_calm() {
        let s = settings();
        assert_eq!(s.classify(&sample(1000, 30)), Level::Normal);
        assert_eq!(s.classify(&sample(13000, 84)), Level::Normal);
    }

    #[test]
    fn classify_degrades_on_either_signal() {
        let s = settings();
        assert_eq!(s.classify(&sample(13107, 30)), Level::Soft, "进程软阈值（40% of 32GB）");
        assert_eq!(s.classify(&sample(1000, 85)), Level::Soft, "系统软阈值");
        assert_eq!(s.classify(&sample(18022, 30)), Level::Hard, "进程硬阈值（55%）");
        assert_eq!(s.classify(&sample(1000, 91)), Level::Hard, "系统硬阈值");
        assert_eq!(s.classify(&sample(22937, 10)), Level::Paused, "进程暂停阈值（70%）");
        assert_eq!(s.classify(&sample(10, 96)), Level::Paused, "系统暂停阈值");
    }

    #[test]
    fn classify_uses_the_worst_signal() {
        let s = settings();
        assert_eq!(s.classify(&sample(19000, 30)), Level::Hard);
        assert_eq!(s.classify(&sample(10, 99)), Level::Paused);
    }

    #[test]
    fn disabled_settings_never_degrade() {
        let s = LoadSettings { enabled: false, ..settings() };
        assert_eq!(s.classify(&sample(u64::MAX, 100)), Level::Normal);
    }

    #[test]
    fn limits_shrink_monotonically_and_stay_above_one() {
        let s = settings();
        let n = s.limits_for(Level::Normal);
        let soft = s.limits_for(Level::Soft);
        let hard = s.limits_for(Level::Hard);
        let paused = s.limits_for(Level::Paused);

        assert!(soft.page_window < n.page_window);
        assert!(hard.page_window < soft.page_window);
        // OCR 许可：软档现在等于正常档（8 = 引擎池上限，实测降到 4 会白吃 ~30% 吞吐），
        // 所以只要求"不高于"；硬档必须真的更小。
        assert!(soft.ocr_permits <= n.ocr_permits);
        assert!(hard.ocr_permits < soft.ocr_permits);
        assert!(hard.ocr_permits >= 1, "硬档也不能为 0（0 表示暂停）");
        assert_eq!(n.doc_permits, 0, "正常档不限制在飞文档");
        assert!(soft.doc_permits > hard.doc_permits);
        assert_eq!(paused.page_window, 0);
        assert_eq!(paused.ocr_permits, 0);
    }

    #[test]
    fn settings_map_parses_and_clamps_nonsense() {
        let mut map = std::collections::HashMap::new();
        map.insert("load_process_soft_pct".to_string(), "70".to_string());
        map.insert("load_process_hard_pct".to_string(), "20".to_string()); // inverted
        map.insert("load_soft_ocr_permits".to_string(), "999".to_string()); // absurd
        map.insert("load_hard_ocr_permits".to_string(), "7".to_string());
        map.insert("adaptive_load_enabled".to_string(), "0".to_string());
        map.insert("load_sample_secs".to_string(), "0".to_string());
        let s = LoadSettings::from_map(&map);
        assert!(!s.enabled);
        assert!(s.process_hard_pct >= s.process_soft_pct, "硬阈值必须 ≥ 软阈值");
        assert!(s.process_pause_pct >= s.process_hard_pct);
        assert!(s.process_soft_pct <= 95, "软阈值不得离谱到无法触发系统信号");
        assert!(s.soft_ocr_permits <= MAX_OCR_PERMITS);
        assert!(s.hard_ocr_permits <= s.soft_ocr_permits, "硬档不得高于软档");
        assert!(s.sample_secs >= 1);
    }

    #[test]
    fn degrade_immediately_recover_slowly() {
        let s = LoadSettings { upgrade_samples: 3, ..settings() };
        // 正常 → 硬：一步到位，不留缓冲
        let (lvl, streak) = next_level(Level::Normal, &sample(19000, 20), &s, 0);
        assert_eq!(lvl, Level::Hard);
        assert_eq!(streak, 0);
        // 硬 → 正常：必须连续多个平静样本，且每次只升一档
        let (lvl, streak) = next_level(Level::Hard, &sample(100, 20), &s, 0);
        assert_eq!(lvl, Level::Hard, "第一次平静不升档");
        assert_eq!(streak, 1);
        let (lvl, streak) = next_level(lvl, &sample(100, 20), &s, streak);
        assert_eq!(lvl, Level::Hard, "样本不够时不升档");
        assert_eq!(streak, 2);
        let (lvl, streak) = next_level(lvl, &sample(100, 20), &s, streak);
        assert_eq!(lvl, Level::Soft, "攒够样本后只升一档");
        assert_eq!(streak, 0);
    }

    #[test]
    fn a_single_busy_sample_resets_the_recovery_streak() {
        let s = LoadSettings { upgrade_samples: 3, ..settings() };
        let (lvl, streak) = next_level(Level::Hard, &sample(100, 20), &s, 0);
        assert_eq!(streak, 1);
        // 刚平静一次又回到硬档阈值 → 计数清零、不升档
        let (lvl, streak) = next_level(lvl, &sample(19000, 20), &s, streak);
        assert_eq!(lvl, Level::Hard, "仍在硬档压力下不得升档");
        assert_eq!(streak, 0, "计数必须清零");
    }

    #[test]
    fn pause_escalates_and_recovers_in_steps() {
        let s = settings();
        let (lvl, _) = next_level(Level::Soft, &sample(23000, 20), &s, 0);
        assert_eq!(lvl, Level::Paused, "暂停阈值直达");
        let (lvl, _) = next_level(lvl, &sample(10, 10), &s, s.upgrade_samples);
        assert_eq!(lvl, Level::Hard, "恢复只升一档");
    }

    /// The gate must actually block while paused and unblock when it calms.
    ///
    /// Uses a **local** `Gate`: the process-wide one is shared by every test in
    /// this binary, and cargo runs them in parallel (one test's `apply_level`
    /// used to release another test's waiter).
    #[test]
    fn permit_gate_blocks_while_paused_then_resumes() {
        let s = settings();
        let g = Gate::new();
        g.apply(Level::Paused, &s);
        assert_eq!(g.page_window.load(Ordering::Relaxed), 0, "暂停时不派发新页");

        std::thread::scope(|scope| {
            let h = scope.spawn(|| {
                let _p = g.acquire(Kind::Ocr);
            });
            std::thread::sleep(Duration::from_millis(120));
            assert!(!h.is_finished(), "暂停时 OCR 许可必须阻塞");

            g.apply(Level::Normal, &s);
            let t0 = Instant::now();
            while !h.is_finished() && t0.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(h.is_finished(), "恢复后阻塞的许可必须被放行");
        });
    }

    /// Throttling must cap concurrency, and dropping a permit must free a slot.
    #[test]
    fn doc_permits_cap_concurrency_and_release_on_drop() {
        let s = LoadSettings { soft_doc_permits: 2, ..settings() };
        let g = Gate::new();
        g.apply(Level::Soft, &s);
        assert_eq!(g.doc_max.load(Ordering::Relaxed), 2);

        std::thread::scope(|scope| {
            let a = g.acquire(Kind::Doc);
            let b = g.acquire(Kind::Doc);
            let h = scope.spawn(|| {
                let _c = g.acquire(Kind::Doc);
            });
            std::thread::sleep(Duration::from_millis(100));
            assert!(!h.is_finished(), "超过 2 个并发文档时必须等待");
            drop(a);
            let t0 = Instant::now();
            while !h.is_finished() && t0.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(h.is_finished(), "释放一个槽位后等待者应当进入");
            drop(b);
        });
    }

    #[test]
    fn normal_level_does_not_limit_documents() {
        let g = Gate::new(); // 默认即正常档
        assert_eq!(g.doc_max.load(Ordering::Relaxed), 0, "正常档不限在飞文档");
        let _a = g.acquire(Kind::Doc);
        let _b = g.acquire(Kind::Doc);
        let _c = g.acquire(Kind::Doc);
        assert_eq!(g.page_window.load(Ordering::Relaxed), page_window_default());
    }

    #[test]
    fn hysteresis_band_stops_flapping_at_the_threshold() {
        // 32GB 机器、进程阈值 40/55/70%、回滞带 5 个百分点 → 软阈值实际要求 <35%（11,468MB）
        let s = LoadSettings { upgrade_samples: 1, recovery_margin_pct: 5, ..settings() };
        let (lvl, streak) = next_level(Level::Soft, &sample(13100, 30), &s, 0);
        assert_eq!(lvl, Level::Soft, "贴着阈值（13,100MB ≥ 35%）不算平静，必须留在软档");
        assert_eq!(streak, 0);
        let (lvl, _) = next_level(Level::Soft, &sample(11000, 30), &s, 0);
        assert_eq!(lvl, Level::Normal, "明显低于回滞带（11,000MB < 11,468MB）才允许升档");
    }

    #[test]
    fn page_window_scales_with_active_passes() {
        // 单文档吃满闸门；两文档均分；低于下限时用下限；暂停档不派发。
        assert_eq!(window_for(4, 8, 1), 8);
        assert_eq!(window_for(4, 8, 2), 4);
        assert_eq!(window_for(4, 8, 3), 4, "均分低于下限时用下限");
        assert_eq!(window_for(4, 8, 0), 8, "计数为 0 视为 1");
        assert_eq!(window_for(0, 8, 1), 0, "暂停档不派发新页");
    }

    #[test]
    fn sampling_reads_plausible_numbers() {
        let s = super::sample();
        assert!(s.process_ws_mb > 0, "本进程 WS 应当可读，实测 {}", s.process_ws_mb);
        assert!(s.total_mb > 1024, "物理内存应当可读，实测 {}MB", s.total_mb);
        assert!(s.system_used_pct <= 100);
    }
}
