//! 飞书 bot 的**生命周期**（`docs/LARK_BOT.md` §5.4 的 C3 —— 文档里标为"唯一必做"）。
//!
//! 由 GUI 拉起 bot（`docs/LARK_BOT.md` §5.1「最优形态：GUI 拉起 bot sidecar」），
//! 应用退出时把它收掉。三条设计要点：
//!
//! 1. **可执行文件按"与主程序同目录"解析**（`current_exe()` 的旁边）。
//!    开发下 `cargo` 已把两个 bin 都放进 `target/debug/`，所以**无需** `externalBin`
//!    就能跑 —— 这很重要：`externalBin` 要求 `<name>-<target-triple>` 文件存在，
//!    配置了却没准备文件会**直接打断 `tauri dev`**。打包时再补那一步。
//! 2. **退出时 kill bot 即可**，不需要给它发信号：bot 持有 lark-cli 的 stdin 管道，
//!    bot 一死该管道即 EOF，lark-cli 会**优雅退出**（它自己的提示就说了 "close stdin"）。
//!    这条路已实测：kill bot 后 lark-cli 消费者退出、**bus daemon 存活**。
//! 3. **不主动停 bus daemon**（按决策"留着复用"）：它是跨 app 单例，杀掉既有订阅泄漏风险，
//!    又会迫使下次启动重连。留着它，下次 bot 起来直接复用。

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

// W5：`lark-bot` 是 **console 子系统**程序，而 GUI 进程没有控制台 —— Windows 会为它
// **新建一个控制台窗口**（用户看到黑框一闪）。`CREATE_NO_WINDOW` 让子进程无窗口运行。
#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CreateProcess` 的 `CREATE_NO_WINDOW` 标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 已拉起的 bot 子进程。`None` ＝ 没在跑。
static BOT: Mutex<Option<Child>> = Mutex::new(None);

/// bot 的可执行文件名（与主程序同目录）。
fn bot_file_name() -> &'static str {
    if cfg!(windows) { "lark-bot.exe" } else { "lark-bot" }
}

/// 可执行文件后缀（用于容错匹配）。
fn bot_ext() -> &'static str {
    if cfg!(windows) { ".exe" } else { "" }
}

/// 解析 bot 可执行文件路径：**主程序旁边**。
///
/// 两种命名都要认，否则打包后会找不到：
/// - 开发：`cargo` 产出 `lark-bot[.exe]`（无后缀）
/// - 打包：`bundle.externalBin` 的命名要求是 `lark-bot-<target-triple>[.exe]`
///   （Tauri 在装包时可能保留也可能去掉三元组后缀 → 所以这里按前缀扫一遍兜住）
fn bot_exe_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe 失败: {e}"))?;
    let dir = exe.parent().ok_or_else(|| "主程序没有父目录".to_string())?;

    let plain = dir.join(bot_file_name());
    if plain.is_file() {
        return Ok(plain);
    }

    let ext = bot_ext();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("lark-bot") && name.ends_with(ext) && entry.path().is_file() {
                return Ok(entry.path());
            }
        }
    }

    Err(format!(
        "在 {} 里没找到 lark-bot{}（开发下跑 `npm run sidecar` 生成；打包需 bundle.externalBin）",
        dir.display(),
        ext
    ))
}

/// 解析 `lark-cli` 可执行文件 —— 委托给**依赖中心**（`deps::catalog::lark_cli_path`）。
///
/// 为什么在依赖中心：这才是"某个外部依赖装没装"该待的地方（与 `is_ready` 同类），
/// 于是"发现 lark-cli"只有一份实现。这里保留同名薄封装，是因为**接缝**就在本文件：
/// 应用解析出路径 → 经 `LS_LARK_CLI` 注入给 bot（bot 是独立 crate，自带兜底探测）。
fn lark_cli_path() -> Option<PathBuf> {
    crate::deps::catalog::lark_cli_path()
}

/// 若飞书集成已启用（`config.json` 的 `integrations.lark.enabled`）则拉起 bot。
/// **任何失败都只记日志**，绝不因此影响应用启动。
///
/// 配置来源在 C1 从 DB `lark_bot_enabled` 改为 `config.json` —— 后者是唯一
/// 真相来源，重启进程/重启 bot 都读同一处（见 `docs/LARK_BOT.md` §5.6）。
pub fn spawn_if_enabled() {
    let enabled = crate::config::lark_integration().enabled;
    if !enabled {
        log::info!("[LARK-BOT] 未启用（integrations.lark.enabled = false），不拉起");
        return;
    }

    let path = match bot_exe_path() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[LARK-BOT] {e}");
            return;
        }
    };

    let mut cmd = Command::new(&path);
    cmd
        // 让 bot 看住我们：应用被**硬杀/崩溃**时 `RunEvent::Exit` 不会执行，
        // 没有任何人会收它 —— 不看着父进程就会留下孤儿 bot（2026-10-15 实测确认）。
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        // stdin 保留为管道：它是"关 stdin 即优雅停机"的落点（Windows 没有可用的 SIGTERM）。
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // W5（见文件头注释）：不加这个标志，Windows 上会闪一个黑框。
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    // ── lark-cli 的发现权归**应用侧**（依赖中心），经环境变量交给 bot ────────────────
    // bot 刻意不依赖应用 lib（上一轮拆 crate 的成果），所以两边**不能共享实现**；
    // 这条环境变量就是接缝：应用解析 → 注入 → bot 直接用（bot 自带探测仅作手动运行的兜底）。
    // 也是「依赖中心」将来"显示状态 / 一键安装"的前置。
    match lark_cli_path() {
        Some(cli) => {
            log::info!("[LARK-BOT] 使用 lark-cli = {}", cli.display());
            cmd.env("LS_LARK_CLI", &cli);
        }
        None => log::warn!(
            "[LARK-BOT] 未发现 lark-cli（`LS_LARK_CLI` / npm 全局路径 / PATH 都没有）—— 交给 bot 自行兜底探测"
        ),
    }

    // ── ffmpeg 的发现权同样归应用侧 ─────────────────────────────────────────
    // bot 出站发语音时要把 mp3/wav 转成飞书要求的 Opus；它不依赖应用 lib，
    // 所以由应用解析好路径、经 `LS_FFMPEG` 注入（找不到就不注入，bot 会退回"按文件发"）。
    if let Some(ff) = crate::extractor::audio::ffmpeg_binary() {
        cmd.env("LS_FFMPEG", &ff);
    }

    match cmd.spawn() {
        Ok(mut child) => {
            if let Some(out) = child.stdout.take() {
                forward(out, "lark-bot", false);
            }
            if let Some(err) = child.stderr.take() {
                forward(err, "lark-bot", true);
            }
            log::info!(
                "[LARK-BOT] 已拉起 {} (pid={})",
                path.display(),
                child.id()
            );
            *BOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        }
        Err(e) => log::warn!("[LARK-BOT] 启动失败（{}）：{e}", path.display()),
    }
}

/// 把子进程的一路输出逐行转发进应用日志。
fn forward<R: std::io::Read + Send + 'static>(reader: R, tag: &'static str, is_err: bool) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            log::log!(bot_line_level(&line, is_err), "[{tag}] {line}");
        }
    });
}

/// 子进程一行输出该记到哪个级别。
///
/// bot 是 tracing/log 写到 **stderr** 的（`<时间戳> <LEVEL> <target>: <消息>`），
/// 于是 stderr 里既有 ERROR 也有 INFO。历史实现把 stderr **一律**记 WARN ——
/// bot 的每条 INFO 都在日志里显示成 WARN，`grep WARN` 时噪声极多、真告警被淹没
/// （2026-10-18 查看 dev 日志时实测）。
///
/// 这里**只看行首前几个 token** 还原真实级别：机器人自身输出、以及它转发的
/// lark-cli 输出都带同一层 tracing 前缀，故第一条就能命中。认不出级别时退回
/// 保守行为（stderr → WARN / stdout → INFO），不会把未知错误悄悄降级成 INFO。
fn bot_line_level(line: &str, is_err: bool) -> log::Level {
    for tok in line.split_whitespace().take(3) {
        let tok = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        match tok.to_ascii_uppercase().as_str() {
            "ERROR" | "ERR" | "FATAL" => return log::Level::Error,
            "WARN" | "WARNING" => return log::Level::Warn,
            "INFO" => return log::Level::Info,
            "DEBUG" => return log::Level::Debug,
            "TRACE" => return log::Level::Trace,
            _ => {}
        }
    }
    if is_err {
        log::Level::Warn
    } else {
        log::Level::Info
    }
}

/// 收掉 bot（应用退出时调用）。
///
/// **只 kill bot，不去动 lark-cli 的 bus daemon** —— 见模块注释第 2、3 点。
pub fn shutdown() {
    let mut slot = BOT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut child) = slot.take() {
        match child.kill() {
            Ok(()) => log::info!("[LARK-BOT] 已停止（pid={}）", child.id()),
            Err(e) => log::warn!("[LARK-BOT] 停止失败：{e}"),
        }
        let _ = child.wait();
    }
}

/// 重启 bot：**先停再按当前配置拉起**。
///
/// 为什么需要它：策略是 bot **启动时读一次**的（`GET /api/integrations/lark`），
/// 之后不会自己发现 `config.json` 变了。所以"改设置即生效"与"手改文件热重载"
/// 两条路都汇到这里。
///
/// 未启用时等价于 [`shutdown`] —— 这一点很重要：关掉开关必须**真的停掉**在跑的旧 bot。
pub fn restart() {
    shutdown();
    spawn_if_enabled();
}

/// 看门狗停机标志：应用**退出**时置位，看门狗不再拉起（[`stop_watchdog`]）。
///
/// 不能复用 [`shutdown`]：`restart()` 也会调它 —— 那只是"改设置重拉"，看门狗必须继续工作。
static WATCHDOG_STOP: AtomicBool = AtomicBool::new(false);

/// bot **看门狗**：意外退出时自动拉起（指数退避）。
///
/// 为什么需要（2026-10-18 通讯审计发现）：bot 在消息长连接结束时**整个退出**
/// （`main` 返回），而应用侧原先只在"改设置 / 配置热重载 / 应用启动"时拉起它 ——
/// 一次网络抖动就可能让机器人离线到用户重启应用。看门狗每 30s 检查一次
/// （连续失败按 [`watchdog_delay`] 退避），机器人重新跑起来后计数归零。
///
/// 不变量：**只做"拉起"，从不 kill** —— 正在跑的 bot 由 `is_running()` 判定后直接跳过，
/// 与设置页的 `restart()` 不冲突。
pub fn spawn_watchdog() {
    std::thread::spawn(move || {
        let mut failures: u32 = 0;
        loop {
            std::thread::sleep(watchdog_delay(failures));
            if WATCHDOG_STOP.load(Ordering::Acquire) {
                return;
            }
            if !crate::config::lark_integration().enabled {
                failures = 0;
                continue;
            }
            if is_running() {
                failures = 0;
                continue;
            }
            log::warn!(
                "[LARK-BOT] 看门狗：机器人未在运行 → 重新拉起（第 {} 次）",
                failures + 1
            );
            spawn_if_enabled();
            failures = failures.saturating_add(1);
        }
    });
}

/// 应用退出时停掉看门狗（在 [`shutdown`] **之前**调用，避免"边退边拉"）。
pub fn stop_watchdog() {
    WATCHDOG_STOP.store(true, Ordering::Release);
}

/// 看门狗检查间隔（纯函数，便于单测）：正常 30s；连续失败指数退避，300s 封顶。
fn watchdog_delay(failures: u32) -> Duration {
    let secs = 30u64.saturating_mul(1u64 << failures.min(4)).min(300);
    Duration::from_secs(secs)
}

/// bot 进程当前是否**在跑**（顺手回收已退出的子进程）。
///
/// 为什么要 `try_wait` 而不是只看 `Option`：bot 可能**自己退出**（读到
/// `enabled = false`、lark-cli 缺失、崩溃…），若不回收，`Some(child)` 会一直
/// 在，状态显示就会撒谎（"运行中"）。回收后由下一次 `restart` / `spawn_if_enabled`
/// 决定要不要再拉起。
pub fn is_running() -> bool {
    let mut slot = BOT.lock().unwrap_or_else(|e| e.into_inner());
    let Some(child) = slot.as_mut() else {
        return false;
    };
    match child.try_wait() {
        Ok(None) => true,
        Ok(Some(_)) => {
            *slot = None;
            false
        }
        // 查询失败（权限/句柄异常）：无法证明它死了，保守当"在跑"。
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{bot_line_level, watchdog_delay};
    use log::Level;
    use std::time::Duration;

    /// 看门狗退避：正常 30s；连续失败翻倍，300s 封顶（且不因计数过大溢出）。
    #[test]
    fn watchdog_backoff_is_exponential_and_capped() {
        assert_eq!(watchdog_delay(0), Duration::from_secs(30));
        assert_eq!(watchdog_delay(1), Duration::from_secs(60));
        assert_eq!(watchdog_delay(2), Duration::from_secs(120));
        assert_eq!(watchdog_delay(3), Duration::from_secs(240));
        assert_eq!(watchdog_delay(4), Duration::from_secs(300), "封顶 300s");
        assert_eq!(watchdog_delay(99), Duration::from_secs(300), "不溢出、仍封顶");
    }

    /// 转发级别按行首 token 还原：bot / lark-cli 的 tracing 前缀都以 `<时间戳> <LEVEL>` 开头。
    #[test]
    fn forwarded_line_level_follows_tracing_prefix() {
        let info = "2026-10-18T15:31:05Z INFO  lark_bot] [bot] 长连接就绪";
        let warn = "2026-10-18T15:31:23Z WARN  lark_bot] [bot] 菜单事件未订阅";
        let err = "2026-10-18T15:31:23Z ERROR lark_bot] [bot] 发送失败";
        // 两层前缀：bot 转发 lark-cli 的输出时，先出现 bot 自己的级别。
        let nested = "2026-10-18T15:31:08Z INFO  lark_bot] [lark-cli] [source] connected";

        assert_eq!(bot_line_level(info, true), Level::Info, "stderr 里的 INFO 不再冒充 WARN");
        assert_eq!(bot_line_level(warn, true), Level::Warn);
        assert_eq!(bot_line_level(err, true), Level::Error);
        assert_eq!(bot_line_level(nested, true), Level::Info);
        assert_eq!(bot_line_level("INFO lark_bot] [bot] 无时间戳也能认出", true), Level::Info);
        assert_eq!(bot_line_level("ERROR: 冒号/标点应被忽略", false), Level::Error);
    }

    /// 认不出级别时保持旧行为（stderr → WARN / stdout → INFO），且消息正文里的级别词不算数。
    #[test]
    fn forwarded_line_level_falls_back_without_prefix() {
        assert_eq!(bot_line_level("{\"msg_type\":\"event\"}", true), Level::Warn);
        assert_eq!(bot_line_level("plain stdout line", false), Level::Info);
        // "ERROR" 出现在第 4 个 token 之后（消息正文）→ 不应抢走级别判定。
        assert_eq!(
            bot_line_level("2026-10-18T15:31:05Z INFO  lark_bot] [bot] 捕获 ERROR 并重试", true),
            Level::Info
        );
    }
}
