//! 后台维护线程：让设置页里那几个开关**真的生效**。
//!
//! 2026-10-18 审计发现三个"死设置"：设置页能改、写进 `app_settings`，但**没有任何
//! 代码读它们**（"自动备份"因此从来没有备份过，用户却以为有备份可恢复）：
//!
//! * `scan_time`（定时扫描时间，默认 `02:00`）—— 另有 seed 写 `scheduled_scan_time`
//!   这个**不同的键**，同一个开关存在两个键名（迁移见 `db::migrate_legacy_backup_settings`；
//!   设置页读写的、`ALLOWED_KEYS` 允许的都是 `scan_time`）。
//! * `auto_backup` + `backup_interval`（天）—— 旧 seed 写 `"1"`，而设置页的勾选框按
//!   `== "true"` 判断 → 显示为**关**。现在 seed 写 `"false"`（默认关：备份要落盘，
//!   未经用户勾选不该自己跑），判定也只认 `"true"`，与界面所见一致。
//!
//! 顺带承担 watcher 事件队列溢出后的**补扫**：队列满了会丢事件（见
//! `scanner::watcher::EVENT_QUEUE_CAP`），这里用同一条全量比对路径把丢掉的变更补回。
//!
//! 线程每 [`TICK_SECS`] 秒醒一次，所有动作都**先抢 `is_scanning`**，与手动扫描/删除
//! 目录后的续扫互斥；任何一步失败只记日志，不影响其余步骤与下一次轮询。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::Manager as _;

use crate::state::AppState;

/// 维护轮询间隔（秒）。定时扫描按分钟粒度判定，60s 足够。
const TICK_SECS: u64 = 60;

/// 自动备份上次成功时间的设置键（我们自己写，不在设置页暴露）。
const LAST_BACKUP_KEY: &str = "last_auto_backup_at";
/// 上次定时扫描日期的设置键（我们自己写）。
const LAST_SCAN_KEY: &str = "last_scheduled_scan_date";

/// 启动维护线程。`app` 需已 `manage(AppState)`。
///
/// `startup_scan_done`：启动扫描（含启动期 VACUUM）跑完前，这一轮什么都不做 ——
/// 启动扫描**没有**抢 `is_scanning`（历史如此），若维护线程同时开扫，两个 walker
/// 会同时走同一批目录并抢 OCR/索引资源。
pub fn start(app: tauri::AppHandle, startup_scan_done: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        log::info!("[MAINT] 后台维护线程已启动（{TICK_SECS}s 轮询）");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(TICK_SECS));
            if !startup_scan_done.load(Ordering::Acquire) {
                log::debug!("[MAINT] 启动扫描尚未结束，跳过本轮维护");
                continue;
            }
            run_once(&app);
        }
    });
}

/// 一轮维护：溢出补扫 → 定时扫描 → 自动备份。
fn run_once(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();

    // ① watcher 事件队列溢出过 → 补一次扫描（丢的只是"通知"，数据在磁盘上还在）。
    if state.watcher.as_ref().is_some_and(|w| w.take_overflow()) {
        let dropped = state.watcher.as_ref().map(|w| w.dropped_events()).unwrap_or(0);
        trigger_scan(
            app,
            &state,
            &format!("watcher 事件队列溢出（累计丢弃 {dropped} 条）"),
        );
    }

    // ② 定时扫描
    if scan_due_now(&state) {
        trigger_scan(app, &state, "定时扫描（scan_time）");
        // 不论扫描是否被"已在扫描"挡下，都把今天记为已处理：否则每分钟都会重试，
        // 把日志刷爆。真要补扫，用户手动点一次即可。
        write_setting(&state, LAST_SCAN_KEY, &chrono::Local::now().format("%Y-%m-%d").to_string());
    }

    // ③ 自动备份
    if backup_due_now(&state) {
        run_backup(app);
    }
}

/// 用与「手动扫描全部」**完全相同**的路径跑一次扫描：进度事件、标志复位、扫描后
/// 的向量/分块回填都在 [`ScanWorker::run`] 里。
///
/// 调用方必须先抢到 `is_scanning`（`ScanWorker::run` 只负责释放）—— 与
/// `commands::index::trigger_scan` 的契约一致。
fn trigger_scan(app: &tauri::AppHandle, state: &AppState, reason: &str) {
    if state.is_restoring.load(Ordering::Acquire) {
        log::info!("[MAINT] 正在恢复备份，跳过后台扫描（{reason}）");
        return;
    }
    if state
        .is_scanning
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        log::info!("[MAINT] 已有扫描在跑，跳过本次后台扫描（{reason}）");
        return;
    }
    log::info!("[MAINT] 触发后台扫描（{reason}）");
    let worker = crate::commands::index::ScanWorker {
        app: app.clone(),
        db: state.db.clone(),
        scanner: state.scanner.clone(),
        indexer: state.indexer.clone(),
        is_scanning: state.is_scanning.clone(),
        cancel_scan: state.cancel_scan.clone(),
        scan_delta: state.scan_delta.clone(),
        logs_dir: state.data_dir.join("logs"),
    };
    std::thread::spawn(move || worker.run(None));
}

// ---------------------------------------------------------------- 定时扫描

/// 现在是否该跑定时扫描了。
fn scan_due_now(state: &AppState) -> bool {
    let Some(scheduled) = read_setting(state, "scan_time") else {
        return false;
    };
    let now = chrono::Local::now();
    scan_due(
        &now.format("%Y-%m-%d").to_string(),
        &now.format("%H:%M").to_string(),
        scheduled.trim(),
        read_setting(state, LAST_SCAN_KEY).as_deref(),
    )
}

/// "今天的定时扫描是否该跑了"（纯函数，便于单测）。
///
/// * `scheduled` 非法或为空 → `false`（清空时间即关闭该功能）
/// * 今天已经跑过（`last_run_date == today`）→ `false`
fn scan_due(today: &str, now_hm: &str, scheduled: &str, last_run_date: Option<&str>) -> bool {
    if last_run_date == Some(today) {
        return false;
    }
    match parse_hm(scheduled) {
        Some(when) => parse_hm(now_hm).is_some_and(|now| now >= when),
        None => false,
    }
}

/// 解析 `"HH:MM"`（容忍 `"2:00"`），非法返回 `None`。
fn parse_hm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

// ---------------------------------------------------------------- 自动备份

/// 现在是否该跑自动备份了。
fn backup_due_now(state: &AppState) -> bool {
    let enabled = read_setting(state, "auto_backup")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    let interval_days = read_setting(state, "backup_interval")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(7);
    let last = read_setting(state, LAST_BACKUP_KEY).and_then(|v| v.trim().parse::<i64>().ok());
    backup_due(enabled, interval_days, last, chrono::Utc::now().timestamp())
}

/// 是否到达备份间隔（纯函数，便于单测）。`last_ts = None` 表示从未备份过。
fn backup_due(enabled: bool, interval_days: u64, last_ts: Option<i64>, now_ts: i64) -> bool {
    if !enabled || interval_days == 0 {
        return false;
    }
    let interval_secs = (interval_days as i64).saturating_mul(86_400);
    match last_ts {
        None => true,
        Some(t) => now_ts.saturating_sub(t) >= interval_secs,
    }
}

/// 跑一次备份（复用 Web/IPC 同一条 `trigger_backup`）。
///
/// 失败**不**记 `last_auto_backup_at`：下一次轮询会重试（间隔本就以天计，重试成本可忽略）。
fn run_backup(app: &tauri::AppHandle) {
    {
        let state = app.state::<AppState>();
        if state.is_restoring.load(Ordering::Acquire) {
            log::info!("[MAINT] 正在恢复备份，跳过本次自动备份");
            return;
        }
        if state.is_scanning.load(Ordering::Acquire) {
            // 备份要读库快照，扫描正在写；下一次轮询再来。
            log::info!("[MAINT] 正在扫描，跳过本次自动备份");
            return;
        }
        log::info!("[MAINT] 自动备份：间隔已到，开始");
        match tauri::async_runtime::block_on(crate::commands::backup::trigger_backup(state)) {
            Ok(()) => {}
            Err(e) => {
                log::warn!("[MAINT] 自动备份失败（下一轮重试）: {e}");
                return;
            }
        }
    }
    let state = app.state::<AppState>();
    write_setting(&state, LAST_BACKUP_KEY, &chrono::Utc::now().timestamp().to_string());
    log::info!("[MAINT] 自动备份完成");
}

// ---------------------------------------------------------------- 设置读写

fn read_setting(state: &AppState, key: &str) -> Option<String> {
    let conn = match state.db.get() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[MAINT] db error: {e}");
            return None;
        }
    };
    conn.query_row("SELECT value FROM app_settings WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

fn write_setting(state: &AppState, key: &str, value: &str) {
    let conn = match state.db.get() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[MAINT] db error: {e}");
            return;
        }
    };
    if let Err(e) = conn.execute(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    ) {
        log::warn!("[MAINT] 写入设置 {key} 失败: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_due_fires_once_per_day_after_the_configured_time() {
        let today = "2026-10-18";
        // 还没到点 → 不跑
        assert!(!scan_due(today, "01:59", "02:00", None));
        // 到点/已过点 → 跑
        assert!(scan_due(today, "02:00", "02:00", None));
        assert!(scan_due(today, "23:59", "02:00", None));
        // 今天跑过 → 不再跑
        assert!(!scan_due(today, "23:59", "02:00", Some(today)));
        // 昨天跑过 → 今天照跑
        assert!(scan_due(today, "03:00", "02:00", Some("2026-10-17")));
        // 非法/空时间 → 关闭
        for bad in ["", "  ", "abc", "2", "25:00", "02:99", "02:0x"] {
            assert!(!scan_due(today, "23:59", bad, None), "非法值 {bad:?} 应视为关闭");
        }
        // 容错写法 "2:00"
        assert!(scan_due(today, "02:00", "2:00", None));
    }

    #[test]
    fn backup_due_respects_toggle_interval_and_last_run() {
        let day = 86_400i64;
        let now = 1_800_000_000i64;
        // 未开启 / 间隔为 0 → 永不跑
        assert!(!backup_due(false, 7, None, now));
        assert!(!backup_due(true, 0, None, now));
        // 从未备份过 → 立刻跑
        assert!(backup_due(true, 7, None, now));
        // 间隔未到 → 不跑；到了 → 跑
        assert!(!backup_due(true, 7, Some(now - 6 * day), now));
        assert!(backup_due(true, 7, Some(now - 7 * day), now));
        assert!(backup_due(true, 7, Some(now - 30 * day), now));
        // 时钟回拨（last 在未来）不得 panic，且按"未到间隔"处理
        assert!(!backup_due(true, 7, Some(now + 10 * day), now));
    }
}
