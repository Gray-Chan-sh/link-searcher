//! `config.json` 的**热重载**（`docs/LARK_BOT.md` §5.6 —— B 档 = "配置文件为中心、允许手改"）。
//!
//! 目标：用户**手改** `config.json` 后无需重启应用即可生效。
//!
//! 三个必须守住的点（每一条都对应一个真实的坑）：
//!
//! 1. **自写抑制按内容 hash，不按 mtime / 事件本身**：应用自己每次保存都会触发文件事件。
//!    若按事件判定，就会变成"自己改自己"→ **重载风暴**。判定逻辑在
//!    [`crate::config::observe_config_content`]（纯函数，可单测）。
//! 2. **监听目录而不是文件**：我们的原子写是 `rename` 覆盖 —— 监听文件会在替换后失效
//!    （各平台行为还不一致）。监听目录 + 过滤文件名是稳妥做法（`scanner/watcher.rs` 同理）。
//! 3. **绝不 panic、绝不阻塞启动**：watcher 起不来只是"没有热重载"，不能拖垮应用。
//!
//! **生效范围（如实记录，不夸大）**：
//! - 飞书策略 → **重启 bot**（它启动时把策略读进内存，不会自己发现文件变了）。
//! - 其它配置项 → 本来就每次现读（`load_config()` 无缓存），因此天然生效。
//! - `web_api_enabled` / 端口这类**进程级**开关 → 仍需重启应用（本轮不承诺）。
//! - 前端 → 广播 [`CONFIG_CHANGED_EVENT`]，设置页据此刷新（它显示的是自己拉的快照）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use tauri::{AppHandle, Emitter};

/// 去抖窗口：一次原子写会连发多个事件（临时文件创建 / rename…），去抖后只看最终结果。
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Tauri 事件名：`config.json` 被**外部**改动并已被接受。
pub const CONFIG_CHANGED_EVENT: &str = "config-changed";

/// 起一个后台线程监听 `config.json` 所在目录。
///
/// **任何失败都只记日志**：没有热重载 ≠ 应用坏了（同 `lark_bot::spawn_if_enabled` 的姿态）。
pub fn spawn(app: AppHandle) {
    let path = crate::config::config_file_path();
    let Some(dir) = path.parent().map(PathBuf::from) else {
        log::warn!("[CONFIG-WATCH] 配置路径没有父目录，热重载不可用");
        return;
    };

    // 先建立基线：把"启动时磁盘上已有的内容"记为已知，否则第一次文件事件无论是什么
    // 都会被当成外部改动（而我们可能只是刚写完默认配置）。
    if let Ok(content) = std::fs::read_to_string(&path) {
        let _ = crate::config::observe_config_content(&content);
    }

    std::thread::spawn(move || {
        let watch_path = path.clone();
        let mut debouncer = match new_debouncer(
            DEBOUNCE,
            None,
            move |result: DebounceEventResult| {
                let Ok(events) = result else { return };
                // 只看"路径命中 config.json"的事件；目录里还有 config.lock 等。
                let hit = events
                    .iter()
                    .any(|d| d.event.paths.iter().any(|p| p == &watch_path));
                if hit {
                    handle_change(&app, &watch_path);
                }
            },
        ) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("[CONFIG-WATCH] 创建热重载监听失败（手改 config.json 将不会自动生效）: {e}");
                return;
            }
        };

        if let Err(e) = debouncer.watch(&dir, RecursiveMode::NonRecursive) {
            log::warn!(
                "[CONFIG-WATCH] 监听 {} 失败（手改 config.json 将不会自动生效）: {e}",
                dir.display()
            );
            return;
        }
        log::info!("[CONFIG-WATCH] 已监听 {}（手改 config.json 后自动生效）", dir.display());

        // 让本线程永久持有 `debouncer`：它一被 drop，监听就没了。
        loop {
            std::thread::park();
        }
    });
}

/// 处理一次去抖后的 `config.json` 变化事件。
fn handle_change(app: &AppHandle, path: &Path) {
    let Ok(content) = std::fs::read_to_string(path) else {
        // 可能正卡在 rename 的窗口里（文件短暂不在）—— 不是错误，下一次事件再来。
        return;
    };
    let Some(change) = crate::config::observe_config_content(&content) else {
        // 我们自己的写，或解析失败（手改写到一半）—— 都静默跳过。
        return;
    };

    log::info!(
        "[CONFIG-WATCH] 检测到 config.json 外部改动 → 已重载（飞书策略{}）",
        if change.lark_changed { "有变化 → 重启 bot" } else { "未变" }
    );

    if change.lark_changed {
        crate::lark_bot::restart();
    }

    if let Err(e) = app.emit(CONFIG_CHANGED_EVENT, serde_json::json!({ "source": "external" })) {
        log::debug!("[CONFIG-WATCH] 广播 {CONFIG_CHANGED_EVENT} 失败: {e}");
    }
}
