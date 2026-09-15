//! Hardware detection and auto-optimization for indexing performance.

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
    pub tier: String, // "high", "balanced", "conservative"
}

/// Detect hardware capabilities: CPU cores and disk speed.
#[tauri::command]
pub fn detect_hardware() -> Result<HardwareInfo, String> {
    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4);

    let disk_result = benchmark_disk_speed();
    let disk_type = classify_disk(disk_result.write_ms, disk_result.read_ms);

    let platform = if cfg!(target_os = "macos") {
        "macos".to_string()
    } else if cfg!(target_os = "windows") {
        "windows".to_string()
    } else {
        "linux".to_string()
    };

    let avg_mbps = (disk_result.write_mbps + disk_result.read_mbps) / 2.0;

    Ok(HardwareInfo {
        cpu_cores,
        disk_speed_mbps: avg_mbps,
        disk_type,
        platform,
    })
}

/// Auto-optimize: detect hardware, compute best settings, and persist them.
#[tauri::command]
pub async fn auto_optimize(state: State<'_, AppState>) -> Result<PerformanceProfile, String> {
    let hw = detect_hardware()?;
    let profile = compute_optimal_profile(&hw);

    // Persist to app_settings.
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let updates = [
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

    // Apply to running IndexerService immediately.
    state.indexer.set_batch_io_concurrency(profile.batch_io_concurrency);
    state.indexer.set_commit_interval(profile.commit_interval);

    log::info!(
        "[PERF] Auto-optimized: tier={}, cores={}, disk={}, concurrency={}, commit_interval={}, buffer={}MB",
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

    let concurrency = get_val("perf_batch_io_concurrency")
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let commit_interval = get_val("perf_commit_interval")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let buffer_mb = get_val("perf_writer_buffer_mb")
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);

    // Determine tier from values.
    let tier = if concurrency >= 20 && commit_interval >= 1500 {
        "high"
    } else if concurrency >= 10 && commit_interval >= 500 {
        "balanced"
    } else {
        "conservative"
    };

    Ok(PerformanceProfile {
        batch_io_concurrency: concurrency,
        commit_interval,
        writer_buffer_mb: buffer_mb,
        tier: tier.to_string(),
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

struct DiskBenchResult {
    write_ms: f64,
    read_ms: f64,
    write_mbps: f64,
    read_mbps: f64,
}

/// Quick disk speed benchmark: write then read a 2 MB temp file.
fn benchmark_disk_speed() -> DiskBenchResult {
    let size = 2 * 1024 * 1024; // 2 MB
    let data = vec![0xABu8; size];

    let tmp = std::env::temp_dir().join(".ls_perf_bench");
    let write_ms;
    let write_mbps;

    {
        let start = std::time::Instant::now();
        if let Ok(mut f) = std::fs::File::create(&tmp) {
            use std::io::Write;
            let _ = f.write_all(&data);
            let _ = f.sync_all();
            write_ms = start.elapsed().as_secs_f64() * 1000.0;
            write_mbps = (size as f64 / 1_048_576.0) / (write_ms / 1000.0);
        } else {
            write_ms = 100.0;
            write_mbps = 0.0;
        }
    }

    let read_ms;
    let read_mbps;

    {
        let start = std::time::Instant::now();
        if let Ok(bytes) = std::fs::read(&tmp) {
            read_ms = start.elapsed().as_secs_f64() * 1000.0;
            read_mbps = (bytes.len() as f64 / 1_048_576.0) / (read_ms / 1000.0);
        } else {
            read_ms = 100.0;
            read_mbps = 0.0;
        }
    }

    let _ = std::fs::remove_file(&tmp);

    DiskBenchResult { write_ms, read_ms, write_mbps, read_mbps }
}

fn classify_disk(write_ms: f64, read_ms: f64) -> String {
    let avg = (write_ms + read_ms) / 2.0;
    if avg < 15.0 {
        "nvme".to_string()
    } else if avg < 80.0 {
        "ssd".to_string()
    } else {
        "hdd".to_string()
    }
}

/// Compute optimal performance profile based on detected hardware.
fn compute_optimal_profile(hw: &HardwareInfo) -> PerformanceProfile {
    let cores = hw.cpu_cores;
    let is_fast_disk = hw.disk_type == "nvme" || hw.disk_type == "ssd";

    let (concurrency, commit_interval, buffer_mb, tier) = if cores >= 16 && is_fast_disk {
        // High-end: NVMe + many cores
        (24, 2000, 300, "high")
    } else if cores >= 8 && is_fast_disk {
        // Balanced: SSD + decent cores
        (12, 1000, 200, "balanced")
    } else if cores >= 4 {
        // Conservative: few cores or HDD
        (4, 500, 150, "conservative")
    } else {
        // Very low-end
        (2, 200, 100, "conservative")
    };

    PerformanceProfile {
        batch_io_concurrency: concurrency,
        commit_interval,
        writer_buffer_mb: buffer_mb,
        tier: tier.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_disk_nvme() {
        assert_eq!(classify_disk(5.0, 3.0), "nvme");
    }

    #[test]
    fn test_classify_disk_ssd() {
        assert_eq!(classify_disk(30.0, 40.0), "ssd");
    }

    #[test]
    fn test_classify_disk_hdd() {
        assert_eq!(classify_disk(100.0, 150.0), "hdd");
    }

    #[test]
    fn test_compute_profile_high_end() {
        let hw = HardwareInfo {
            cpu_cores: 16,
            disk_speed_mbps: 3000.0,
            disk_type: "nvme".to_string(),
            platform: "macos".to_string(),
        };
        let p = compute_optimal_profile(&hw);
        assert_eq!(p.tier, "high");
        assert_eq!(p.batch_io_concurrency, 24);
    }

    #[test]
    fn test_compute_profile_balanced() {
        let hw = HardwareInfo {
            cpu_cores: 8,
            disk_speed_mbps: 500.0,
            disk_type: "ssd".to_string(),
            platform: "macos".to_string(),
        };
        let p = compute_optimal_profile(&hw);
        assert_eq!(p.tier, "balanced");
        assert_eq!(p.batch_io_concurrency, 12);
    }

    #[test]
    fn test_compute_profile_conservative() {
        let hw = HardwareInfo {
            cpu_cores: 2,
            disk_speed_mbps: 100.0,
            disk_type: "hdd".to_string(),
            platform: "windows".to_string(),
        };
        let p = compute_optimal_profile(&hw);
        assert_eq!(p.tier, "conservative");
        assert_eq!(p.batch_io_concurrency, 2);
    }

    #[test]
    fn test_benchmark_disk_runs() {
        let r = benchmark_disk_speed();
        // Just ensure it doesn't panic and returns reasonable values.
        assert!(r.write_ms >= 0.0);
        assert!(r.read_ms >= 0.0);
    }
}
