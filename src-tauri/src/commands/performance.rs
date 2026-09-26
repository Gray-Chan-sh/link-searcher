//! Hardware detection and auto-optimization for indexing performance.

use std::path::Path;

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct HardwareInfo {
    pub cpu_cores: u32,
    pub disk_speed_mbps: f64,
    pub disk_type: String, // "nvme", "ssd", "hdd", "unknown"
    pub platform: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PerformanceProfile {
    pub batch_io_concurrency: usize,
    pub commit_interval: usize,
    pub writer_buffer_mb: usize,
    pub tier: String, // "aggressive", "balanced", "conservative"
}

/// 32 MB per sample: large enough that per-file/syscall overhead and timer
/// granularity no longer dominate, small enough to stay a quick probe.
const BENCH_SAMPLE_BYTES: usize = 32 * 1024 * 1024;
/// Number of timed runs; the median is used so a single stall (AV scan, other
/// process IO) does not skew the result.
const BENCH_RUNS: usize = 3;

/// Detect hardware capabilities: CPU cores and disk speed.
#[tauri::command]
pub fn detect_hardware(state: State<'_, AppState>) -> Result<HardwareInfo, String> {
    Ok(detect_hardware_inner(&state.data_dir))
}

/// Auto-optimize: detect hardware, pick the recommended tier, and persist it.
#[tauri::command]
pub async fn auto_optimize(state: State<'_, AppState>) -> Result<PerformanceProfile, String> {
    let hw = detect_hardware_inner(&state.data_dir);
    let tier = recommend_tier(&hw);
    let profile = scale_params(tier, hw.cpu_cores, &hw.disk_type);

    persist_profile(&state, &profile)?;
    apply_profile(&state, &profile);

    log::info!(
        "[PERF] Auto-optimized: tier={}, cores={}, disk={}, concurrency={}, commit_interval={}, buffer={}MB",
        profile.tier, hw.cpu_cores, hw.disk_type, profile.batch_io_concurrency, profile.commit_interval, profile.writer_buffer_mb
    );

    Ok(profile)
}

/// Apply a user-selected tier. The tier sets the intent; concrete parameters
/// are scaled to the detected hardware, so the same tier yields different
/// values on different machines.
#[tauri::command]
pub async fn set_performance_tier(
    state: State<'_, AppState>,
    tier: String,
) -> Result<PerformanceProfile, String> {
    let Some(tier) = parse_tier(&tier) else {
        return Err(format!("unknown performance tier: {tier}"));
    };

    let hw = detect_hardware_inner(&state.data_dir);
    let profile = scale_params(tier, hw.cpu_cores, &hw.disk_type);

    persist_profile(&state, &profile)?;
    apply_profile(&state, &profile);

    log::info!(
        "[PERF] Tier set: tier={}, cores={}, disk={}, concurrency={}, commit_interval={}, buffer={}MB",
        profile.tier, hw.cpu_cores, hw.disk_type, profile.batch_io_concurrency, profile.commit_interval, profile.writer_buffer_mb
    );

    Ok(profile)
}

/// Get the current performance profile (from DB settings or defaults).
#[tauri::command]
pub fn get_performance_profile(state: State<'_, AppState>) -> Result<PerformanceProfile, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let get_val = |key: &str| -> Option<String> {
        conn.query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get::<_, String>(0),
        )
        .ok()
    };

    let stored_tier = get_val("perf_tier");
    let has_values = get_val("perf_batch_io_concurrency").is_some();
    let concurrency = get_val("perf_batch_io_concurrency")
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let commit_interval = get_val("perf_commit_interval")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let buffer_mb = get_val("perf_writer_buffer_mb")
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);

    // Prefer the persisted tier; fall back to deriving it from stored values
    // (pre-tier installs), or "balanced" when nothing was ever configured.
    let tier = match stored_tier.as_deref() {
        Some(raw) => parse_tier(raw)
            .unwrap_or_else(|| derive_tier(concurrency, commit_interval))
            .to_string(),
        None if has_values => derive_tier(concurrency, commit_interval).to_string(),
        None => "balanced".to_string(),
    };

    Ok(PerformanceProfile {
        batch_io_concurrency: concurrency,
        commit_interval,
        writer_buffer_mb: buffer_mb,
        tier,
    })
}

/// Get writer buffer size from settings (used by boot.rs at startup).
pub fn get_writer_buffer_mb_from_db(
    conn: &rusqlite::Connection,
) -> usize {
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = 'perf_writer_buffer_mb'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(150)
}

/// Get batch IO concurrency from settings (used by boot.rs at startup).
pub fn get_batch_io_concurrency_from_db(
    conn: &rusqlite::Connection,
) -> usize {
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = 'perf_batch_io_concurrency'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(8)
}

/// Get commit interval from settings (used by boot.rs at startup).
pub fn get_commit_interval_from_db(
    conn: &rusqlite::Connection,
) -> usize {
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = 'perf_commit_interval'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(100)
}

// ── Internal helpers ──────────────────────────────────────────────────

fn detect_hardware_inner(data_dir: &Path) -> HardwareInfo {
    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4);

    let disk_speed_mbps = benchmark_disk_write(data_dir);
    let disk_type = classify_disk(disk_speed_mbps);

    let platform = if cfg!(target_os = "macos") {
        "macos".to_string()
    } else if cfg!(target_os = "windows") {
        "windows".to_string()
    } else {
        "linux".to_string()
    };

    HardwareInfo {
        cpu_cores,
        disk_speed_mbps,
        disk_type,
        platform,
    }
}

/// Sequential write throughput (MB/s) into `dir`, which should be the data
/// directory so the result reflects the disk that actually hosts the index.
///
/// The file is written, flushed (`sync_all`) and closed on each run; the
/// median of [`BENCH_RUNS`] timed runs is returned. Reads are intentionally
/// not measured: the freshly written file lives in the OS page cache, so a
/// read would report memory bandwidth rather than disk speed.
fn benchmark_disk_write(dir: &Path) -> f64 {
    let data = vec![0xABu8; BENCH_SAMPLE_BYTES];
    let tmp = dir.join(format!(".ls_perf_bench.{}.tmp", std::process::id()));

    let measure_once = || -> Option<f64> {
        use std::io::Write;

        let start = std::time::Instant::now();
        let mut file = std::fs::File::create(&tmp).ok()?;
        file.write_all(&data).ok()?;
        file.sync_all().ok()?;
        drop(file);

        let secs = start.elapsed().as_secs_f64();
        let _ = std::fs::remove_file(&tmp);

        if secs <= 0.0 {
            return None;
        }
        Some((BENCH_SAMPLE_BYTES as f64 / 1_048_576.0) / secs)
    };

    // Warm-up run: primes the directory metadata and the OS/AV write path.
    let _ = measure_once();

    let mut samples: Vec<f64> = (0..BENCH_RUNS).filter_map(|_| measure_once()).collect();
    let _ = std::fs::remove_file(&tmp);

    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    samples[samples.len() / 2]
}

fn classify_disk(write_mbps: f64) -> String {
    if write_mbps <= 0.0 {
        "unknown".to_string()
    } else if write_mbps < 150.0 {
        "hdd".to_string()
    } else if write_mbps < 800.0 {
        "ssd".to_string()
    } else {
        "nvme".to_string()
    }
}

/// Map a tier name to its canonical form. `high` is accepted as a legacy
/// alias of `aggressive`.
fn parse_tier(tier: &str) -> Option<&'static str> {
    match tier {
        "conservative" => Some("conservative"),
        "balanced" => Some("balanced"),
        "aggressive" | "high" => Some("aggressive"),
        _ => None,
    }
}

/// Disk throughput factor applied to the worker count.
fn disk_factor(disk_type: &str) -> f64 {
    match disk_type {
        "nvme" => 1.0,
        "ssd" => 0.75,
        "hdd" => 0.35,
        _ => 0.5,
    }
}

/// Scale a tier's parameters to the detected hardware. The tier only sets the
/// relative intent; the absolute worker count follows CPU cores and disk
/// class, so the same tier yields different values on different machines.
fn scale_params(tier: &str, cores: u32, disk_type: &str) -> PerformanceProfile {
    let (mult, floor, ceil, commit_interval, writer_buffer_mb): (f64, u32, u32, usize, usize) =
        match tier {
            "conservative" => (0.5, 1, 8, 500, 100),
            "aggressive" => (1.5, 4, 24, 2000, 300),
            _ => (1.0, 2, 16, 1000, 200),
        };

    let raw = (cores as f64 * mult * disk_factor(disk_type)).round() as i64;
    let concurrency = raw.clamp(floor as i64, ceil as i64) as usize;

    PerformanceProfile {
        batch_io_concurrency: concurrency,
        commit_interval,
        writer_buffer_mb,
        tier: tier.to_string(),
    }
}

/// Pick the tier that best matches the machine.
fn recommend_tier(hw: &HardwareInfo) -> &'static str {
    let fast_disk = hw.disk_type == "nvme" || hw.disk_type == "ssd";
    if hw.cpu_cores >= 16 && fast_disk {
        "aggressive"
    } else if hw.cpu_cores >= 8 && fast_disk {
        "balanced"
    } else {
        "conservative"
    }
}

/// Backwards-compatible tier derivation from raw parameter values.
fn derive_tier(concurrency: usize, commit_interval: usize) -> &'static str {
    if concurrency >= 20 && commit_interval >= 1500 {
        "aggressive"
    } else if concurrency >= 10 && commit_interval >= 500 {
        "balanced"
    } else {
        "conservative"
    }
}

fn persist_profile(state: &State<'_, AppState>, profile: &PerformanceProfile) -> Result<(), String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let updates = [
        ("perf_tier", profile.tier.clone()),
        ("perf_batch_io_concurrency", profile.batch_io_concurrency.to_string()),
        ("perf_commit_interval", profile.commit_interval.to_string()),
        ("perf_writer_buffer_mb", profile.writer_buffer_mb.to_string()),
    ];
    for (key, value) in &updates {
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .map_err(|e| format!("failed to save '{key}': {e}"))?;
    }
    Ok(())
}

fn apply_profile(state: &State<'_, AppState>, profile: &PerformanceProfile) {
    state.indexer.set_batch_io_concurrency(profile.batch_io_concurrency);
    state.indexer.set_commit_interval(profile.commit_interval);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_disk_nvme() {
        assert_eq!(classify_disk(2000.0), "nvme");
    }

    #[test]
    fn test_classify_disk_ssd() {
        assert_eq!(classify_disk(400.0), "ssd");
    }

    #[test]
    fn test_classify_disk_hdd() {
        assert_eq!(classify_disk(100.0), "hdd");
    }

    #[test]
    fn test_classify_disk_unknown() {
        assert_eq!(classify_disk(0.0), "unknown");
    }

    #[test]
    fn test_parse_tier() {
        assert_eq!(parse_tier("aggressive"), Some("aggressive"));
        assert_eq!(parse_tier("high"), Some("aggressive"));
        assert_eq!(parse_tier("balanced"), Some("balanced"));
        assert_eq!(parse_tier("conservative"), Some("conservative"));
        assert_eq!(parse_tier("bogus"), None);
    }

    #[test]
    fn test_scale_same_tier_different_machines() {
        // 同一档位在不同机器上得到不同并行度。
        assert_eq!(scale_params("balanced", 16, "nvme").batch_io_concurrency, 16);
        assert_eq!(scale_params("balanced", 8, "ssd").batch_io_concurrency, 6);
        assert_eq!(scale_params("balanced", 4, "hdd").batch_io_concurrency, 2);
    }

    #[test]
    fn test_scale_tiers_on_same_machine() {
        assert_eq!(scale_params("conservative", 16, "nvme").batch_io_concurrency, 8);
        assert_eq!(scale_params("balanced", 16, "nvme").batch_io_concurrency, 16);
        assert_eq!(scale_params("aggressive", 16, "nvme").batch_io_concurrency, 24);
    }

    #[test]
    fn test_scale_clamps_hdd_aggressive() {
        // HDD 上激进档也必须压住并行度，避免寻道抖动。
        let p = scale_params("aggressive", 4, "hdd");
        assert_eq!(p.batch_io_concurrency, 4);
    }

    #[test]
    fn test_recommend_tier() {
        let nvme16 = HardwareInfo {
            cpu_cores: 16,
            disk_speed_mbps: 2000.0,
            disk_type: "nvme".to_string(),
            platform: "macos".to_string(),
        };
        assert_eq!(recommend_tier(&nvme16), "aggressive");

        let ssd8 = HardwareInfo {
            cpu_cores: 8,
            disk_speed_mbps: 400.0,
            disk_type: "ssd".to_string(),
            platform: "windows".to_string(),
        };
        assert_eq!(recommend_tier(&ssd8), "balanced");

        let hdd4 = HardwareInfo {
            cpu_cores: 4,
            disk_speed_mbps: 100.0,
            disk_type: "hdd".to_string(),
            platform: "windows".to_string(),
        };
        assert_eq!(recommend_tier(&hdd4), "conservative");
    }

    #[test]
    fn test_derive_tier() {
        assert_eq!(derive_tier(24, 2000), "aggressive");
        assert_eq!(derive_tier(12, 1000), "balanced");
        assert_eq!(derive_tier(4, 500), "conservative");
    }

    #[test]
    fn test_benchmark_disk_runs() {
        let r = benchmark_disk_write(&std::env::temp_dir());
        // Just ensure it doesn't panic and returns a finite, non-negative value.
        assert!(r >= 0.0 && r.is_finite());
    }
}
